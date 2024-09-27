#![deny(warnings)]

use {
    anyhow::{anyhow, bail, ensure, Context, Error, Result},
    async_trait::async_trait,
    bytes::Bytes,
    component_init::Invoker,
    futures::future::FutureExt,
    heck::ToSnakeCase,
    indexmap::{IndexMap, IndexSet},
    prelink::{embedded_helper_utils, embedded_python_standard_library},
    serde::Deserialize,
    std::{
        collections::HashMap,
        env, fs,
        io::Cursor,
        iter,
        ops::Deref,
        path::{Path, PathBuf},
        str,
    },
    summary::{Escape, Locations, Summary},
    wasm_convert::IntoValType,
    wasm_encoder::{
        CodeSection, ExportKind, ExportSection, Function, FunctionSection, Instruction, Module,
        TypeSection,
    },
    wasmparser::{FuncType, Parser, Payload, TypeRef},
    wasmtime::{
        component::{Component, Instance, Linker, ResourceTable, ResourceType},
        Config, Engine, Store,
    },
    wasmtime_wasi::{
        pipe::{MemoryInputPipe, MemoryOutputPipe},
        DirPerms, FilePerms, WasiCtx, WasiCtxBuilder, WasiView,
    },
    wit_parser::{Resolve, TypeDefKind, UnresolvedPackageGroup, WorldId, WorldItem, WorldKey},
};

mod abi;
mod bindgen;
mod bindings;
pub mod command;
mod prelink;
#[cfg(feature = "pyo3")]
mod python;
mod summary;
#[cfg(test)]
mod test;
mod util;

wasmtime::component::bindgen!({
    path: "wit",
    world: "init",
    async: true
});

pub struct Ctx {
    wasi: WasiCtx,
    table: ResourceTable,
}

pub struct Library {
    name: String,
    module: Vec<u8>,
    dl_openable: bool,
}

impl WasiView for Ctx {
    fn ctx(&mut self) -> &mut WasiCtx {
        &mut self.wasi
    }
    fn table(&mut self) -> &mut ResourceTable {
        &mut self.table
    }
}

#[derive(Deserialize)]
struct RawComponentizePyConfig {
    bindings: Option<String>,
    wit_directory: Option<String>,
}

#[derive(Debug)]
struct ComponentizePyConfig {
    bindings: Option<PathBuf>,
    wit_directory: Option<PathBuf>,
}

impl TryFrom<(&Path, RawComponentizePyConfig)> for ComponentizePyConfig {
    type Error = Error;

    fn try_from((path, raw): (&Path, RawComponentizePyConfig)) -> Result<Self> {
        let base = path.canonicalize()?;
        let convert = |p| {
            // Ensure this is a relative path under `base`:
            let p = base.join(p);
            let p = p.canonicalize().with_context(|| p.display().to_string())?;
            ensure!(p.starts_with(&base));
            Ok(p)
        };

        Ok(Self {
            bindings: raw.bindings.map(convert).transpose()?,
            wit_directory: raw.wit_directory.map(convert).transpose()?,
        })
    }
}

#[derive(Debug)]
pub struct ConfigContext<T> {
    module: String,
    root: PathBuf,
    path: PathBuf,
    config: T,
}

struct MyInvoker {
    store: Store<Ctx>,
    instance: Instance,
}

#[async_trait]
impl Invoker for MyInvoker {
    async fn call_s32(&mut self, function: &str) -> Result<i32> {
        let func = self
            .instance
            .get_typed_func::<(), (i32,)>(&mut self.store, function)?;
        let result = func.call_async(&mut self.store, ()).await?.0;
        func.post_return_async(&mut self.store).await?;
        Ok(result)
    }

    async fn call_s64(&mut self, function: &str) -> Result<i64> {
        let func = self
            .instance
            .get_typed_func::<(), (i64,)>(&mut self.store, function)?;
        let result = func.call_async(&mut self.store, ()).await?.0;
        func.post_return_async(&mut self.store).await?;
        Ok(result)
    }

    async fn call_float32(&mut self, function: &str) -> Result<f32> {
        let func = self
            .instance
            .get_typed_func::<(), (f32,)>(&mut self.store, function)?;
        let result = func.call_async(&mut self.store, ()).await?.0;
        func.post_return_async(&mut self.store).await?;
        Ok(result)
    }

    async fn call_float64(&mut self, function: &str) -> Result<f64> {
        let func = self
            .instance
            .get_typed_func::<(), (f64,)>(&mut self.store, function)?;
        let result = func.call_async(&mut self.store, ()).await?.0;
        func.post_return_async(&mut self.store).await?;
        Ok(result)
    }

    async fn call_list_u8(&mut self, function: &str) -> Result<Vec<u8>> {
        let func = self
            .instance
            .get_typed_func::<(), (Vec<u8>,)>(&mut self.store, function)?;
        let result = func.call_async(&mut self.store, ()).await?.0;
        func.post_return_async(&mut self.store).await?;
        Ok(result)
    }
}

pub fn generate_bindings(
    wit_path: &Path,
    world: Option<&str>,
    world_module: Option<&str>,
    output_dir: &Path,
) -> Result<()> {
    // TODO: Split out and reuse the code responsible for finding and using componentize-py.toml files in the
    // `componentize` function below, since that can affect the bindings we should be generating.

    let (resolve, world) = parse_wit(wit_path, world)?;
    let summary = Summary::try_new(&resolve, &iter::once(world).collect())?;
    let world_name = resolve.worlds[world].name.to_snake_case().escape();
    let world_module = world_module.unwrap_or(&world_name);
    let world_dir = output_dir.join(world_module.replace('.', "/"));
    fs::create_dir_all(&world_dir)?;
    summary.generate_code(
        &world_dir,
        world,
        world_module,
        &mut Locations::default(),
        true,
    )?;

    Ok(())
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub async fn componentize(
    wit_path: Option<&Path>,
    world: Option<&str>,
    python_path: &[&str],
    module_worlds: &[(&str, &str)],
    app_name: &str,
    output_path: &Path,
    add_to_linker: Option<&dyn Fn(&mut Linker<Ctx>) -> Result<()>>,
    stub_wasi: bool,
) -> Result<()> {
    // Remove non-existent elements from `python_path` so we don't choke on them later:
    let python_path = &python_path
        .iter()
        .filter_map(|&s| Path::new(s).exists().then_some(s))
        .collect::<Vec<_>>();

    let embedded_python_standard_lib = embedded_python_standard_library()?;
    let embedded_helper_utils = embedded_helper_utils()?;

    let (configs, mut libraries) =
        prelink::search_for_libraries_and_configs(python_path, module_worlds, world)?;

    // Next, iterate over all the WIT directories, merging them into a single `Resolve`, and matching Python
    // packages to `WorldId`s.
    let (mut resolve, mut main_world) = if let Some(path) = wit_path {
        let (resolve, world) = parse_wit(path, world)?;
        (Some(resolve), Some(world))
    } else {
        (None, None)
    };

    let configs = configs
        .iter()
        .map(|(module, (config, world))| {
            Ok((module, match (world, config.config.wit_directory.as_deref()) {
                (_, Some(wit_path)) => {
                    let (my_resolve, mut world) = parse_wit(&config.path.join(wit_path), *world)?;

                    if let Some(resolve) = &mut resolve {
                        let remap = resolve.merge(my_resolve)?;
                        world = remap.worlds[world.index()].expect("missing world");
                    } else {
                        resolve = Some(my_resolve);
                    }

                    (config, Some(world))
                }
                (None, None) => (config, None),
                (Some(_), None) => {
                    bail!("no `wit-directory` specified in `componentize-py.toml` for module `{module}`");
                }
            }))
        })
        .collect::<Result<IndexMap<_, _>>>()?;

    let resolve = if let Some(resolve) = resolve {
        resolve
    } else {
        // If no WIT directory was provided as a parameter and none were referenced by Python packages, use ./wit
        // by default.
        let (my_resolve, world) = parse_wit(Path::new("wit"), world).context(
            "no WIT files found; please specify the directory or file \
             containing the WIT world you wish to target",
        )?;
        main_world = Some(world);
        my_resolve
    };

    // Extract relevant metadata from the `Resolve` into a `Summary` instance, which we'll use to generate Wasm-
    // and Python-level bindings.

    let worlds = configs
        .values()
        .filter_map(|(_, world)| *world)
        .chain(main_world)
        .collect::<IndexSet<_>>();

    let summary = Summary::try_new(&resolve, &worlds)?;

    libraries.push(Library {
        name: "libcomponentize_py_bindings.so".into(),
        module: bindings::make_bindings(&resolve, &worlds, &summary)?,
        dl_openable: false,
    });

    // Link all the libraries (including any native extensions) into a single component.
    let mut linker = wit_component::Linker::default().validate(true);

    let mut wasi_imports = HashMap::new();
    for Library {
        name,
        module,
        dl_openable,
    } in &libraries
    {
        if stub_wasi {
            add_wasi_imports(module, &mut wasi_imports)?;
        }
        linker = linker.library(name, module, *dl_openable)?;
    }

    linker = linker.adapter(
        "wasi_snapshot_preview1",
        &zstd::decode_all(Cursor::new(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/wasi_snapshot_preview1.reactor.wasm.zst"
        ))))?,
    )?;

    let component = linker.encode()?;

    let stubbed_component = if stub_wasi {
        // When `stub_wasi` is `true`, we apply the pre-initialization snapshot to an alternate version of the
        // component -- one where the WASI imports have been stubbed out.

        let mut linker = wit_component::Linker::default().validate(true);

        for Library {
            name,
            module,
            dl_openable,
        } in &libraries
        {
            linker = linker.library(name, module, *dl_openable)?;
        }

        for (module, imports) in &wasi_imports {
            linker = linker.adapter(module, &make_stub_adapter(module, imports))?;
        }

        let component = linker.encode()?;

        // As of this writing, `wit_component::Linker` generates a component such that the first module is the
        // `main` one, followed by any adapters, followed by any libraries, followed by the `init` module, which is
        // finally followed by any shim modules.  Given that the stubbed component may contain more adapters than
        // the non-stubbed version, we need to tell `component-init` how to translate module indexes from the
        // former to the latter.
        //
        // TODO: this is pretty fragile in that it could silently break if `wit_component::Linker`'s implementation
        // changes.  Can we make it more robust?

        let old_adapter_count = 1;
        let new_adapter_count = u32::try_from(wasi_imports.len()).unwrap();
        assert!(new_adapter_count >= old_adapter_count);

        Some((component, move |index: u32| {
            if index == 0 {
                // `main` module
                0
            } else if index <= new_adapter_count {
                // adapter module
                old_adapter_count
            } else {
                // one of the other kinds of module
                index + old_adapter_count - new_adapter_count
            }
        }))
    } else {
        None
    };

    // Pre-initialize the component by running it through `component_init::initialize`.  Currently, this is the
    // application's first and only chance to load any standard or third-party modules since we do not yet include
    // a virtual filesystem in the component to make those modules available at runtime.

    let stdout = MemoryOutputPipe::new(10000);
    let stderr = MemoryOutputPipe::new(10000);

    let mut wasi = WasiCtxBuilder::new();
    wasi.stdin(MemoryInputPipe::new(Bytes::new()))
        .stdout(stdout.clone())
        .stderr(stderr.clone())
        .env("PYTHONUNBUFFERED", "1")
        .env("COMPONENTIZE_PY_APP_NAME", app_name)
        .env("PYTHONHOME", "/python")
        .preopened_dir(
            embedded_python_standard_lib.path(),
            "python",
            DirPerms::all(),
            FilePerms::all(),
        )?
        .preopened_dir(
            embedded_helper_utils.path(),
            "bundled",
            DirPerms::all(),
            FilePerms::all(),
        )?;

    // Generate guest mounts for each host directory in `python_path`.
    for (index, path) in python_path.iter().enumerate() {
        wasi.preopened_dir(path, index.to_string(), DirPerms::all(), FilePerms::all())?;
    }

    // For each Python package with a `componentize-py.toml` file that specifies where generated bindings for that
    // package should be placed, generate the bindings and place them as indicated.

    let mut world_dir_mounts = Vec::new();
    let mut locations = Locations::default();
    let mut saw_main_world = false;

    for (config, world, binding_path) in configs
        .values()
        .filter_map(|(config, world)| Some((config, world, config.config.bindings.as_deref()?)))
    {
        if *world == main_world {
            saw_main_world = true;
        }

        let Some(world) = *world else {
            bail!("please specify a world for module `{}`", config.module);
        };

        let paths = python_path
            .iter()
            .enumerate()
            .map(|(index, dir)| {
                let dir = Path::new(dir).canonicalize()?;
                Ok(if config.root == dir {
                    config
                        .path
                        .join(binding_path)
                        .strip_prefix(dir)
                        .ok()
                        .map(|p| (index, p.to_str().unwrap().replace('\\', "/")))
                } else {
                    None
                })
            })
            .filter_map(Result::transpose)
            .collect::<Result<Vec<_>>>()?;

        let binding_module = paths.first().unwrap().1.replace('/', ".");

        let world_dir = tempfile::tempdir()?;

        summary.generate_code(
            world_dir.path(),
            world,
            &binding_module,
            &mut locations,
            false,
        )?;

        world_dir_mounts.push((
            paths
                .iter()
                .map(|(index, p)| format!("{index}/{p}"))
                .collect(),
            world_dir,
        ));
    }

    // If the caller specified a world and we haven't already generated bindings for it above, do so now.
    if let (Some(world), false) = (main_world, saw_main_world) {
        let module = resolve.worlds[world].name.to_snake_case();
        let world_dir = tempfile::tempdir()?;
        let module_path = world_dir.path().join(&module);
        fs::create_dir_all(&module_path)?;
        summary.generate_code(&module_path, world, &module, &mut locations, false)?;
        world_dir_mounts.push((vec!["world".to_owned()], world_dir));

        // The helper utilities are hard-coded to assume the world module is named `proxy`.  Here we replace that
        // with the actual world name.
        fn replace(path: &Path, pattern: &str, replacement: &str) -> Result<()> {
            if path.is_dir() {
                for entry in fs::read_dir(path)? {
                    replace(&entry?.path(), pattern, replacement)?;
                }
            } else {
                fs::write(
                    path,
                    fs::read_to_string(path)?
                        .replace(pattern, replacement)
                        .as_bytes(),
                )?;
            }

            Ok(())
        }
        replace(embedded_helper_utils.path(), "proxy", &module)?;
    };

    for (mounts, world_dir) in world_dir_mounts.iter() {
        for mount in mounts {
            wasi.preopened_dir(world_dir.path(), mount, DirPerms::all(), FilePerms::all())?;
        }
    }

    // Generate a `Symbols` object containing metadata to be passed to the pre-init function.  The runtime library
    // will use this to look up types and functions that will later be referenced by the generated Wasm code.
    let symbols = summary.collect_symbols(&locations);

    // Finally, pre-initialize the component, writing the result to `output_path`.

    let python_path = (0..python_path.len())
        .map(|index| format!("/{index}"))
        .collect::<Vec<_>>()
        .join(":");

    let table = ResourceTable::new();
    let wasi = wasi
        .env(
            "PYTHONPATH",
            format!("/python:/world:{python_path}:/bundled"),
        )
        .build();

    let mut config = Config::new();
    config.wasm_component_model(true);
    config.async_support(true);

    let engine = Engine::new(&config)?;

    let mut linker = Linker::new(&engine);
    let added_to_linker = if let Some(add_to_linker) = add_to_linker {
        add_to_linker(&mut linker)?;
        true
    } else {
        false
    };

    let mut store = Store::new(&engine, Ctx { wasi, table });

    let app_name = app_name.to_owned();
    let component = component_init::initialize_staged(
        &component,
        stubbed_component
            .as_ref()
            .map(|(component, map)| (component.deref(), map as &dyn Fn(u32) -> u32)),
        move |instrumented| {
            async move {
                let component = &Component::new(&engine, instrumented)?;
                if !added_to_linker {
                    add_wasi_and_stubs(&resolve, &worlds, &mut linker)?;
                }

                let pre = InitPre::new(linker.instantiate_pre(component)?)?;
                let instance = pre.instance_pre.instantiate_async(&mut store).await?;
                let guest = pre.interface0.load(&mut store, &instance)?;

                guest
                    .call_init(&mut store, &app_name, &symbols, stub_wasi)
                    .await?
                    .map_err(|e| anyhow!("{e}"))?;

                Ok(Box::new(MyInvoker { store, instance }) as Box<dyn Invoker>)
            }
            .boxed()
        },
    )
    .await
    .with_context(move || {
        format!(
            "{}{}",
            String::from_utf8_lossy(&stdout.try_into_inner().unwrap()),
            String::from_utf8_lossy(&stderr.try_into_inner().unwrap())
        )
    })?;

    fs::write(output_path, component)?;

    Ok(())
}

fn parse_wit(path: &Path, world: Option<&str>) -> Result<(Resolve, WorldId)> {
    let mut resolve = Resolve::default();
    let pkg = if path.is_dir() {
        resolve.push_dir(path)?.0
    } else {
        let pkg = UnresolvedPackageGroup::parse_file(path)?;
        resolve.push_group(pkg)?
    };
    let world = resolve.select_world(pkg, world)?;
    Ok((resolve, world))
}

fn add_wasi_and_stubs(
    resolve: &Resolve,
    worlds: &IndexSet<WorldId>,
    linker: &mut Linker<Ctx>,
) -> Result<()> {
    wasmtime_wasi::add_to_linker_async(linker)?;

    enum Stub<'a> {
        Function(&'a String),
        Resource(&'a String),
    }

    let mut stubs = HashMap::<_, Vec<_>>::new();
    for &world in worlds {
        for (key, item) in &resolve.worlds[world].imports {
            match item {
                WorldItem::Interface { id, .. } => {
                    let interface_name = match key {
                        WorldKey::Name(name) => name.clone(),
                        WorldKey::Interface(interface) => resolve.id_of(*interface).unwrap(),
                    };

                    let interface = &resolve.interfaces[*id];
                    for function_name in interface.functions.keys() {
                        stubs
                            .entry(Some(interface_name.clone()))
                            .or_default()
                            .push(Stub::Function(function_name));
                    }

                    for (type_name, id) in interface.types.iter() {
                        if let TypeDefKind::Resource = &resolve.types[*id].kind {
                            stubs
                                .entry(Some(interface_name.clone()))
                                .or_default()
                                .push(Stub::Resource(type_name));
                        }
                    }
                }
                WorldItem::Function(function) => {
                    stubs
                        .entry(None)
                        .or_default()
                        .push(Stub::Function(&function.name));
                }
                WorldItem::Type(id) => {
                    let ty = &resolve.types[*id];
                    if let TypeDefKind::Resource = &ty.kind {
                        stubs
                            .entry(None)
                            .or_default()
                            .push(Stub::Resource(ty.name.as_ref().unwrap()));
                    }
                }
            }
        }
    }

    for (interface_name, stubs) in stubs {
        if let Some(interface_name) = interface_name {
            if let Ok(mut instance) = linker.instance(&interface_name) {
                for stub in stubs {
                    let interface_name = interface_name.clone();
                    match stub {
                        Stub::Function(name) => instance.func_new(name, {
                            let name = name.clone();
                            move |_, _, _| {
                                Err(anyhow!("called trapping stub: {interface_name}#{name}"))
                            }
                        }),
                        Stub::Resource(name) => instance
                            .resource(name, ResourceType::host::<()>(), {
                                let name = name.clone();
                                move |_, _| {
                                    Err(anyhow!("called trapping stub: {interface_name}#{name}"))
                                }
                            })
                            .map(drop),
                    }?;
                }
            }
        } else {
            let mut instance = linker.root();
            for stub in stubs {
                match stub {
                    Stub::Function(name) => instance.func_new(name, {
                        let name = name.clone();
                        move |_, _, _| Err(anyhow!("called trapping stub: {name}"))
                    }),
                    Stub::Resource(name) => instance
                        .resource(name, ResourceType::host::<()>(), {
                            let name = name.clone();
                            move |_, _| Err(anyhow!("called trapping stub: {name}"))
                        })
                        .map(drop),
                }?;
            }
        }
    }

    Ok(())
}

fn add_wasi_imports<'a>(
    module: &'a [u8],
    imports: &mut HashMap<&'a str, HashMap<&'a str, FuncType>>,
) -> Result<()> {
    let mut types = Vec::new();
    for payload in Parser::new(0).parse_all(module) {
        match payload? {
            Payload::TypeSection(reader) => {
                types = reader
                    .into_iter_err_on_gc_types()
                    .collect::<Result<Vec<_>, _>>()?;
            }

            Payload::ImportSection(reader) => {
                for import in reader {
                    let import = import?;

                    if import.module == "wasi_snapshot_preview1"
                        || import.module.starts_with("wasi:")
                    {
                        if let TypeRef::Func(ty) = import.ty {
                            imports
                                .entry(import.module)
                                .or_default()
                                .insert(import.name, types[usize::try_from(ty).unwrap()].clone());
                        } else {
                            bail!("encountered non-function import from WASI namespace")
                        }
                    }
                }
                break;
            }

            _ => {}
        }
    }

    Ok(())
}

fn make_stub_adapter(_module: &str, stubs: &HashMap<&str, FuncType>) -> Vec<u8> {
    let mut types = TypeSection::new();
    let mut functions = FunctionSection::new();
    let mut exports = ExportSection::new();
    let mut code = CodeSection::new();

    for (index, (name, ty)) in stubs.iter().enumerate() {
        let index = u32::try_from(index).unwrap();
        types.function(
            ty.params().iter().map(|&v| IntoValType(v).into()),
            ty.results().iter().map(|&v| IntoValType(v).into()),
        );
        functions.function(index);
        exports.export(name, ExportKind::Func, index);
        let mut function = Function::new([]);
        function.instruction(&Instruction::Unreachable);
        function.instruction(&Instruction::End);
        code.function(&function);
    }

    let mut module = Module::new();
    module.section(&types);
    module.section(&functions);
    module.section(&exports);
    module.section(&code);

    module.finish()
}

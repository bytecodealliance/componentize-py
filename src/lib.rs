#![deny(warnings)]

use {
    crate::command::WasiAdapter,
    anyhow::{anyhow, bail, ensure, Context, Error, Result},
    async_trait::async_trait,
    bytes::Bytes,
    component_init_transform::Invoker,
    futures::future::FutureExt,
    heck::ToSnakeCase,
    indexmap::{IndexMap, IndexSet},
    serde::Deserialize,
    std::{
        collections::HashMap,
        fs, iter,
        ops::Deref,
        path::{Path, PathBuf},
        str,
    },
    summary::{Escape, Locations, Summary},
    wasmtime::{
        component::{Component, Instance, Linker, ResourceTable, ResourceType},
        Config, Engine, Store,
    },
    wasmtime_wasi::{
        p2::{
            pipe::{MemoryInputPipe, MemoryOutputPipe},
            IoView, WasiCtx, WasiCtxBuilder, WasiView,
        },
        DirPerms, FilePerms,
    },
    wit_parser::{Resolve, TypeDefKind, UnresolvedPackageGroup, WorldId, WorldItem, WorldKey},
};

mod abi;
mod bindgen;
mod bindings;
pub mod command;
mod link;
mod prelink;
#[cfg(feature = "pyo3")]
mod python;
mod stubwasi;
mod summary;
#[cfg(test)]
mod test;
mod util;

/// The default name of the Python module containing code generated from the
/// specified WIT world.  This may be overriden programatically or via the CLI
/// using the `--world-module` option.
static DEFAULT_WORLD_MODULE: &str = "wit_world";

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
}

impl IoView for Ctx {
    fn table(&mut self) -> &mut ResourceTable {
        &mut self.table
    }
}

#[derive(Deserialize)]
struct RawComponentizePyConfig {
    bindings: Option<String>,
    wit_directory: Option<String>,
    #[serde(default)]
    import_interface_names: HashMap<String, String>,
    #[serde(default)]
    export_interface_names: HashMap<String, String>,
}

#[derive(Debug)]
struct ComponentizePyConfig {
    bindings: Option<PathBuf>,
    wit_directory: Option<PathBuf>,
    import_interface_names: HashMap<String, String>,
    export_interface_names: HashMap<String, String>,
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
            import_interface_names: raw.import_interface_names,
            export_interface_names: raw.export_interface_names,
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

    async fn call_f32(&mut self, function: &str) -> Result<f32> {
        let func = self
            .instance
            .get_typed_func::<(), (f32,)>(&mut self.store, function)?;
        let result = func.call_async(&mut self.store, ()).await?.0;
        func.post_return_async(&mut self.store).await?;
        Ok(result)
    }

    async fn call_f64(&mut self, function: &str) -> Result<f64> {
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

#[allow(clippy::too_many_arguments)]
pub fn generate_bindings(
    wit_path: &Path,
    world: Option<&str>,
    features: &[String],
    all_features: bool,
    world_module: Option<&str>,
    output_dir: &Path,
    import_interface_names: &HashMap<&str, &str>,
    export_interface_names: &HashMap<&str, &str>,
) -> Result<()> {
    // TODO: Split out and reuse the code responsible for finding and using componentize-py.toml files in the
    // `componentize` function below, since that can affect the bindings we should be generating.

    let (resolve, world) = parse_wit(wit_path, world, features, all_features)?;
    let summary = Summary::try_new(
        &resolve,
        &iter::once(world).collect(),
        import_interface_names,
        export_interface_names,
    )?;
    let world_module = world_module.unwrap_or(DEFAULT_WORLD_MODULE);
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
    features: &[String],
    all_features: bool,
    world_module: Option<&str>,
    python_path: &[&str],
    module_worlds: &[(&str, &str)],
    app_name: &str,
    output_path: &Path,
    add_to_linker: Option<&dyn Fn(&mut Linker<Ctx>) -> Result<()>>,
    adapter: WasiAdapter,
    stub_wasi: bool,
    import_interface_names: &HashMap<&str, &str>,
    export_interface_names: &HashMap<&str, &str>,
) -> Result<()> {
    // Remove non-existent elements from `python_path` so we don't choke on them later:
    let python_path = &python_path
        .iter()
        .filter_map(|&s| Path::new(s).exists().then_some(s))
        .collect::<Vec<_>>();

    let embedded_python_standard_lib = prelink::embedded_python_standard_library()?;
    let embedded_helper_utils = prelink::embedded_helper_utils()?;

    let (configs, mut libraries) =
        prelink::search_for_libraries_and_configs(python_path, module_worlds, world)?;

    // Next, iterate over all the WIT directories, merging them into a single `Resolve`, and matching Python
    // packages to `WorldId`s.
    let (mut resolve, mut main_world) = if let Some(path) = wit_path {
        let (resolve, world) = parse_wit(path, world, features, all_features)?;
        (Some(resolve), Some(world))
    } else {
        (None, None)
    };

    let import_interface_names = import_interface_names
        .iter()
        .map(|(a, b)| (*a, *b))
        .chain(configs.iter().flat_map(|(_, (config, _))| {
            config
                .config
                .import_interface_names
                .iter()
                .map(|(a, b)| (a.as_str(), b.as_str()))
        }))
        .collect();

    let export_interface_names = export_interface_names
        .iter()
        .map(|(a, b)| (*a, *b))
        .chain(configs.iter().flat_map(|(_, (config, _))| {
            config
                .config
                .export_interface_names
                .iter()
                .map(|(a, b)| (a.as_str(), b.as_str()))
        }))
        .collect();

    let configs = configs
        .iter()
        .map(|(module, (config, world))| {
            Ok((module, match (world, config.config.wit_directory.as_deref()) {
                (_, Some(wit_path)) => {
                    let (my_resolve, mut world) = parse_wit(&config.path.join(wit_path), *world, features, all_features)?;

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
        let (my_resolve, world) = parse_wit(Path::new("wit"), world, features, all_features)
            .context(
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

    if worlds
        .iter()
        .any(|&id| app_name == resolve.worlds[id].name.to_snake_case().escape())
    {
        bail!("App name `{app_name}` conflicts with world name; please rename your application module.");
    }

    let summary = Summary::try_new(
        &resolve,
        &worlds,
        &import_interface_names,
        &export_interface_names,
    )?;

    libraries.push(Library {
        name: "libcomponentize_py_bindings.so".into(),
        module: bindings::make_bindings(&resolve, &worlds, &summary)?,
        dl_openable: false,
    });

    let component = link::link_libraries(&libraries, adapter)?;

    let stubbed_component = if stub_wasi {
        stubwasi::link_stub_modules(libraries)?
    } else {
        None
    };

    // Pre-initialize the component by running it through `component_init_transform::initialize`.
    // Currently, this is the application's first and only chance to load any standard or
    // third-party modules since we do not yet include a virtual filesystem in the component to
    // make those modules available at runtime.

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
        let module = world_module.unwrap_or(DEFAULT_WORLD_MODULE);
        let world_dir = tempfile::tempdir()?;
        let module_path = world_dir.path().join(module);
        fs::create_dir_all(&module_path)?;
        summary.generate_code(&module_path, world, module, &mut locations, false)?;
        world_dir_mounts.push((vec!["world".to_owned()], world_dir));

        // The helper utilities are hard-coded to assume the world module is named `wit_world`.  Here we replace
        // that with the actual world module name.
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
        replace(embedded_helper_utils.path(), "wit_world", module)?;
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
    let component = component_init_transform::initialize_staged(
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
                let guest = pre.indices.interface0.load(&mut store, &instance)?;

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

fn parse_wit(
    path: &Path,
    world: Option<&str>,
    features: &[String],
    all_features: bool,
) -> Result<(Resolve, WorldId)> {
    let mut resolve = Resolve {
        all_features,
        ..Default::default()
    };
    for features in features {
        for feature in features
            .split(',')
            .flat_map(|s| s.split_whitespace())
            .filter(|f| !f.is_empty())
        {
            resolve.features.insert(feature.to_string());
        }
    }
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
    wasmtime_wasi::p2::add_to_linker_async(linker)?;

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
            // Note that we do _not_ stub interfaces which appear to be part of WASIp2 since those should be
            // provided by the `wasmtime_wasi::add_to_linker_async` call above, and adding stubs to those same
            // interfaces would just cause trouble.
            if !is_wasip2_cli(&interface_name) {
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
                                        Err(anyhow!(
                                            "called trapping stub: {interface_name}#{name}"
                                        ))
                                    }
                                })
                                .map(drop),
                        }?;
                    }
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

fn is_wasip2_cli(interface_name: &str) -> bool {
    (interface_name.starts_with("wasi:cli/")
        || interface_name.starts_with("wasi:clocks/")
        || interface_name.starts_with("wasi:random/")
        || interface_name.starts_with("wasi:io/")
        || interface_name.starts_with("wasi:filesystem/")
        || interface_name.starts_with("wasi:sockets/"))
        && interface_name.contains("@0.2.")
}

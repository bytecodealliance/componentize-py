#![deny(warnings)]

use {
    anyhow::{anyhow, bail, ensure, Context, Error, Result},
    async_trait::async_trait,
    bytes::Bytes,
    component_init::Invoker,
    futures::future::FutureExt,
    heck::ToSnakeCase,
    indexmap::{IndexMap, IndexSet},
    serde::Deserialize,
    std::{
        collections::{HashMap, HashSet},
        env, fs,
        io::Cursor,
        iter,
        ops::Deref,
        path::{Path, PathBuf},
        str,
    },
    summary::{Escape, Locations, Summary},
    tar::Archive,
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
        preview2::{
            command as wasi_command,
            pipe::{MemoryInputPipe, MemoryOutputPipe},
            DirPerms, FilePerms, WasiCtx, WasiCtxBuilder, WasiView,
        },
        Dir,
    },
    wit_parser::{Resolve, TypeDefKind, UnresolvedPackage, WorldId, WorldItem, WorldKey},
    zstd::Decoder,
};

mod abi;
mod bindgen;
mod bindings;
pub mod command;
#[cfg(feature = "pyo3")]
mod python;
mod summary;
#[cfg(test)]
mod test;
mod util;

static NATIVE_EXTENSION_SUFFIX: &str = ".cpython-312-wasm32-wasi.so";

wasmtime::component::bindgen!({
    path: "wit",
    world: "init",
    async: true
});

pub struct Ctx {
    wasi: WasiCtx,
    table: ResourceTable,
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
struct ConfigContext<T> {
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
            .exports(&mut self.store)
            .root()
            .typed_func::<(), (i32,)>(function)?;
        let result = func.call_async(&mut self.store, ()).await?.0;
        func.post_return_async(&mut self.store).await?;
        Ok(result)
    }

    async fn call_s64(&mut self, function: &str) -> Result<i64> {
        let func = self
            .instance
            .exports(&mut self.store)
            .root()
            .typed_func::<(), (i64,)>(function)?;
        let result = func.call_async(&mut self.store, ()).await?.0;
        func.post_return_async(&mut self.store).await?;
        Ok(result)
    }

    async fn call_float32(&mut self, function: &str) -> Result<f32> {
        let func = self
            .instance
            .exports(&mut self.store)
            .root()
            .typed_func::<(), (f32,)>(function)?;
        let result = func.call_async(&mut self.store, ()).await?.0;
        func.post_return_async(&mut self.store).await?;
        Ok(result)
    }

    async fn call_float64(&mut self, function: &str) -> Result<f64> {
        let func = self
            .instance
            .exports(&mut self.store)
            .root()
            .typed_func::<(), (f64,)>(function)?;
        let result = func.call_async(&mut self.store, ()).await?.0;
        func.post_return_async(&mut self.store).await?;
        Ok(result)
    }

    async fn call_list_u8(&mut self, function: &str) -> Result<Vec<u8>> {
        let func = self
            .instance
            .exports(&mut self.store)
            .root()
            .typed_func::<(), (Vec<u8>,)>(function)?;
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
    isyswasfa: Option<&str>,
) -> Result<()> {
    // TODO: Split out and reuse the code responsible for finding and using componentize-py.toml files in the
    // `componentize` function below, since that can affect the bindings we should be generating.

    let (mut resolve, world) = parse_wit(wit_path, world)?;
    if let Some(suffix) = isyswasfa {
        isyswasfa_transform::transform(&mut resolve, world, Some(suffix));
    }
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
        isyswasfa,
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
    isyswasfa: Option<&str>,
    stub_wasi: bool,
) -> Result<()> {
    // Remove non-existent elements from `python_path` so we don't choke on them later:
    let python_path = &python_path
        .iter()
        .filter_map(|&s| Path::new(s).exists().then_some(s))
        .collect::<Vec<_>>();

    // Untar the embedded copy of the Python standard library into a temporary directory
    let stdlib = tempfile::tempdir()?;

    Archive::new(Decoder::new(Cursor::new(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/python-lib.tar.zst"
    ))))?)
    .unpack(stdlib.path())?;

    // Untar the embedded copy of helper utilities into a temporary directory
    let bundled = tempfile::tempdir()?;

    Archive::new(Decoder::new(Cursor::new(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/bundled.tar.zst"
    ))))?)
    .unpack(bundled.path())?;

    // Search `python_path` for native extension libraries and/or componentize-py.toml files.  Packages containing
    // the latter may contain their own WIT files defining their own worlds (in addition to what the caller
    // specified as paramters), which we'll try to match up with `module_worlds` in the next step.
    let mut raw_configs = Vec::new();
    let mut library_path = Vec::with_capacity(python_path.len());
    for path in python_path {
        let mut libraries = Vec::new();
        search_directory(
            Path::new(path),
            Path::new(path),
            &mut libraries,
            &mut raw_configs,
            &mut HashSet::new(),
        )?;
        library_path.push((*path, libraries));
    }

    // Validate the paths parsed from any componentize-py.toml files discovered above and match them up with
    // `module_worlds` entries.  Note that we use an `IndexMap` to preserve the order specified in `module_worlds`,
    // which is required to be topologically sorted with respect to package dependencies.
    //
    // For any packages which contain componentize-py.toml files but no corresponding `module_worlds` entry, we use
    // the `world` parameter as a default.
    let configs = {
        let mut configs = raw_configs
            .into_iter()
            .map(|raw_config| {
                let config =
                    ComponentizePyConfig::try_from((raw_config.path.deref(), raw_config.config))?;

                Ok((
                    raw_config.module.clone(),
                    ConfigContext {
                        module: raw_config.module,
                        root: raw_config.root,
                        path: raw_config.path,
                        config,
                    },
                ))
            })
            .collect::<Result<HashMap<_, _>>>()?;

        let mut ordered = IndexMap::new();
        for (module, world) in module_worlds {
            if let Some(config) = configs.remove(*module) {
                ordered.insert((*module).to_owned(), (config, Some(*world)));
            } else {
                bail!("no `componentize-py.toml` file found for module `{module}`");
            }
        }

        for (module, config) in configs {
            ordered.insert(module, (config, world));
        }

        ordered
    };

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
                        world = remap.worlds[world.index()];
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

    let mut resolve = if let Some(resolve) = resolve {
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

    if let Some(suffix) = isyswasfa {
        let mut suffix = Some(suffix);
        for &world in &worlds {
            isyswasfa_transform::transform(&mut resolve, world, suffix);
            suffix = None;
        }
    }

    let summary = Summary::try_new(&resolve, &worlds)?;

    struct Library {
        name: String,
        module: Vec<u8>,
        dl_openable: bool,
    }

    let mut libraries = vec![
        Library {
            name: "libcomponentize_py_runtime.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libcomponentize_py_runtime.so.zst"
            ))))?,
            dl_openable: false,
        },
        Library {
            name: "libpython3.12.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libpython3.12.so.zst"
            ))))?,
            dl_openable: false,
        },
        Library {
            name: "libc.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libc.so.zst"
            ))))?,
            dl_openable: false,
        },
        Library {
            name: "libwasi-emulated-mman.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libwasi-emulated-mman.so.zst"
            ))))?,
            dl_openable: false,
        },
        Library {
            name: "libwasi-emulated-process-clocks.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libwasi-emulated-process-clocks.so.zst"
            ))))?,
            dl_openable: false,
        },
        Library {
            name: "libwasi-emulated-getpid.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libwasi-emulated-getpid.so.zst"
            ))))?,
            dl_openable: false,
        },
        Library {
            name: "libwasi-emulated-signal.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libwasi-emulated-signal.so.zst"
            ))))?,
            dl_openable: false,
        },
        Library {
            name: "libc++.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libc++.so.zst"
            ))))?,
            dl_openable: false,
        },
        Library {
            name: "libc++abi.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libc++abi.so.zst"
            ))))?,
            dl_openable: false,
        },
        Library {
            name: "libcomponentize_py_bindings.so".into(),
            module: bindings::make_bindings(&resolve, &worlds, &summary)?,
            dl_openable: false,
        },
    ];

    for (index, (path, libs)) in library_path.iter().enumerate() {
        for library in libs {
            let path = library
                .strip_prefix(path)
                .unwrap()
                .to_str()
                .context("non-UTF-8 path")?
                .replace('\\', "/");

            libraries.push(Library {
                name: format!("/{index}/{path}"),
                module: fs::read(library)?,
                dl_openable: true,
            });
        }
    }

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
            Dir::open_ambient_dir(stdlib.path(), cap_std::ambient_authority())
                .with_context(|| format!("unable to open {}", stdlib.path().display()))?,
            DirPerms::all(),
            FilePerms::all(),
            "python",
        )
        .preopened_dir(
            Dir::open_ambient_dir(bundled.path(), cap_std::ambient_authority())
                .with_context(|| format!("unable to open {}", bundled.path().display()))?,
            DirPerms::all(),
            FilePerms::all(),
            "bundled",
        );

    // Generate guest mounts for each host directory in `python_path`.
    for (index, path) in python_path.iter().enumerate() {
        wasi.preopened_dir(
            Dir::open_ambient_dir(path, cap_std::ambient_authority())
                .with_context(|| format!("unable to open {path}"))?,
            DirPerms::all(),
            FilePerms::all(),
            &index.to_string(),
        );
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
            isyswasfa,
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
        summary.generate_code(
            &module_path,
            world,
            &module,
            &mut locations,
            false,
            isyswasfa,
        )?;
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
        replace(bundled.path(), "proxy", &module)?;
    };

    for (mounts, world_dir) in world_dir_mounts.iter() {
        for mount in mounts {
            wasi.preopened_dir(
                Dir::open_ambient_dir(world_dir.path(), cap_std::ambient_authority())
                    .with_context(|| format!("unable to open {}", world_dir.path().display()))?,
                DirPerms::all(),
                FilePerms::all(),
                mount,
            );
        }
    }

    // Generate a `Symbols` object containing metadata to be passed to the pre-init function.  The runtime library
    // will use this to look up types and functions that will later be referenced by the generated Wasm code.
    let symbols = summary.collect_symbols(&locations, isyswasfa);

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
                    add_wasi_and_stubs(&resolve, &worlds, component, &mut linker)?;
                }

                let (init, instance) =
                    Init::instantiate_async(&mut store, component, &linker).await?;

                init.exports()
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
        let pkg = UnresolvedPackage::parse_file(path)?;
        resolve.push(pkg)?
    };
    let world = resolve.select_world(pkg, world)?;
    Ok((resolve, world))
}

fn add_wasi_and_stubs(
    resolve: &Resolve,
    worlds: &IndexSet<WorldId>,
    component: &Component,
    linker: &mut Linker<Ctx>,
) -> Result<()> {
    wasi_command::add_to_linker(linker)?;

    enum Stub<'a> {
        Function(&'a String),
        Resource(&'a String),
    }

    let mut stubs = HashMap::<_, Vec<_>>::new();
    for &world in worlds {
        for (key, item) in &resolve.worlds[world].imports {
            match item {
                WorldItem::Interface(interface) => {
                    let interface_name = match key {
                        WorldKey::Name(name) => name.clone(),
                        WorldKey::Interface(interface) => resolve.id_of(*interface).unwrap(),
                    };

                    let interface = &resolve.interfaces[*interface];
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
                        Stub::Function(name) => instance.func_new(component, name, {
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
                    Stub::Function(name) => instance.func_new(component, name, {
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

fn search_directory(
    root: &Path,
    path: &Path,
    libraries: &mut Vec<PathBuf>,
    configs: &mut Vec<ConfigContext<RawComponentizePyConfig>>,
    modules_seen: &mut HashSet<String>,
) -> Result<()> {
    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            search_directory(root, &entry?.path(), libraries, configs, modules_seen)?;
        }
    } else if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
        if name.ends_with(NATIVE_EXTENSION_SUFFIX) {
            libraries.push(path.to_owned());
        } else if name == "componentize-py.toml" {
            let root = root.canonicalize()?;
            let path = path.canonicalize()?;

            let module = module_name(&root, &path)
                .ok_or_else(|| anyhow!("unable to determine module name for {}", path.display()))?;

            let mut push = true;
            for existing in &mut *configs {
                if path == existing.path.join("componentize-py.toml") {
                    // When one directory in `PYTHON_PATH` is a subdirectory of the other, we consider the
                    // subdirectory to be the true owner of the file.  This is important later, when we derive a
                    // package name by stripping the root directory from the file path.
                    if root > existing.root {
                        module.clone_into(&mut existing.module);
                        root.clone_into(&mut existing.root);
                        path.parent().unwrap().clone_into(&mut existing.path);
                    }
                    push = false;
                    break;
                } else {
                    // If we find a componentize-py.toml file under a Python module which will not be used because
                    // we already found a version of that module in an earlier `PYTHON_PATH` directory, we'll
                    // ignore the latest one.
                    //
                    // For example, if the module `foo_sdk` appears twice in `PYTHON_PATH`, and both versions have
                    // a componentize-py.toml file, we'll ignore the second one just as Python will ignore the
                    // second module.

                    if modules_seen.contains(&module) {
                        bail!("multiple `componentize-py.toml` files found in module `{module}`");
                    }

                    modules_seen.insert(module.clone());

                    if module == existing.module {
                        push = false;
                        break;
                    }
                }
            }

            if push {
                configs.push(ConfigContext {
                    module,
                    root: root.to_owned(),
                    path: path.parent().unwrap().to_owned(),
                    config: toml::from_str::<RawComponentizePyConfig>(&fs::read_to_string(path)?)?,
                });
            }
        }
    }

    Ok(())
}

fn module_name(root: &Path, path: &Path) -> Option<String> {
    if let [first, _, ..] = &path.strip_prefix(root).ok()?.iter().collect::<Vec<_>>()[..] {
        first.to_str().map(|s| s.to_owned())
    } else {
        None
    }
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

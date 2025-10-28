#![deny(warnings)]

use {
    anyhow::{Context, Error, Result, anyhow, bail, ensure},
    async_trait::async_trait,
    bytes::Bytes,
    component_init_transform::Invoker,
    futures::future::FutureExt,
    heck::ToSnakeCase,
    indexmap::{IndexMap, IndexSet},
    serde::Deserialize,
    std::{
        borrow::Cow,
        collections::HashMap,
        fs, iter,
        ops::Deref,
        path::{Path, PathBuf},
        str,
    },
    summary::{Escape, Locations, Summary},
    wasm_encoder::{CustomSection, Section as _},
    wasmtime::{
        Config, Engine, Store,
        component::{Component, Instance, Linker, ResourceTable, ResourceType},
    },
    wasmtime_wasi::{
        DirPerms, FilePerms, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView,
        p2::pipe::{MemoryInputPipe, MemoryOutputPipe},
    },
    wit_component::metadata,
    wit_dylib::DylibOpts,
    wit_parser::{
        CloneMaps, Package, PackageName, Resolve, Stability, TypeDefKind, UnresolvedPackageGroup,
        World, WorldId, WorldItem, WorldKey,
    },
};

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

const DEBUG_PYTHON_BINDINGS: bool = false;

/// The default name of the Python module containing code generated from the
/// specified WIT world.  This may be overriden programatically or via the CLI
/// using the `--world-module` option.
static DEFAULT_WORLD_MODULE: &str = "wit_world";

wasmtime::component::bindgen!({
    path: "wit",
    world: "init",
    exports: { default: async },
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
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
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
    wit_path: &[impl AsRef<Path>],
    world: Option<&str>,
    features: &[String],
    all_features: bool,
    world_module: Option<&str>,
    output_dir: &Path,
    import_interface_names: &HashMap<&str, &str>,
    export_interface_names: &HashMap<&str, &str>,
) -> Result<()> {
    // TODO: Split out and reuse the code responsible for finding and using
    // componentize-py.toml files in the `componentize` function below, since
    // that can affect the bindings we should be generating.

    let (resolve, world) = parse_wit(wit_path, world, features, all_features)?;
    let import_function_indexes = &HashMap::new();
    let export_function_indexes = &HashMap::new();
    let stream_and_future_indexes = &HashMap::new();
    let summary = Summary::try_new(
        &resolve,
        &iter::once(world).collect(),
        import_interface_names,
        export_interface_names,
        import_function_indexes,
        export_function_indexes,
        stream_and_future_indexes,
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
    wit_path: &[impl AsRef<Path>],
    world: Option<&str>,
    features: &[String],
    all_features: bool,
    world_module: Option<&str>,
    python_path: &[&str],
    module_worlds: &[(&str, &str)],
    app_name: &str,
    output_path: &Path,
    add_to_linker: Option<&dyn Fn(&mut Linker<Ctx>) -> Result<()>>,
    stub_wasi: bool,
    import_interface_names: &HashMap<&str, &str>,
    export_interface_names: &HashMap<&str, &str>,
) -> Result<()> {
    // Remove non-existent elements from `python_path` so we don't choke on them
    // later:
    let python_path = &python_path
        .iter()
        .filter_map(|&s| Path::new(s).exists().then_some(s))
        .collect::<Vec<_>>();

    let embedded_python_standard_lib = prelink::embedded_python_standard_library()?;
    let embedded_helper_utils = prelink::embedded_helper_utils()?;

    let (configs, libraries) =
        prelink::search_for_libraries_and_configs(python_path, module_worlds, world)?;

    // Next, iterate over all the WIT directories, merging them into a single
    // `Resolve`, and matching Python packages to `WorldId`s.
    let (mut resolve, mut main_world) = match wit_path {
        [] => (None, None),
        paths => {
            let (resolve, world) = parse_wit(paths, world, features, all_features)?;
            (Some(resolve), Some(world))
        }
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
                    let paths = &[config.path.join(wit_path)];
                    let (my_resolve, mut world) = parse_wit(paths, *world, features, all_features)?;

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

    let mut resolve = if let Some(resolve) = resolve {
        resolve
    } else {
        // If no WIT directory was provided as a parameter and none were
        // referenced by Python packages, use the default values.
        let paths: &[&Path] = &[];
        let (my_resolve, world) = parse_wit(paths, world, features, all_features).context(
            "no WIT files found; please specify the directory or file \
             containing the WIT world you wish to target",
        )?;
        main_world = Some(world);
        my_resolve
    };

    // Extract relevant metadata from the `Resolve` into a `Summary` instance,
    // which we'll use to generate Wasm- and Python-level bindings.

    let worlds = configs
        .values()
        .filter_map(|(_, world)| *world)
        .chain(main_world)
        .collect::<IndexSet<_>>();

    if worlds
        .iter()
        .any(|&id| app_name == resolve.worlds[id].name.to_snake_case().escape())
    {
        bail!(
            "App name `{app_name}` conflicts with world name; please rename your application module."
        );
    }

    let union_package = resolve.packages.alloc(Package {
        name: PackageName {
            namespace: "componentize-py".into(),
            name: "union".into(),
            version: None,
        },
        docs: Default::default(),
        interfaces: Default::default(),
        worlds: Default::default(),
    });

    let union_world = resolve.worlds.alloc(World {
        name: "union".into(),
        imports: Default::default(),
        exports: Default::default(),
        package: Some(union_package),
        docs: Default::default(),
        stability: Stability::Unknown,
        includes: Default::default(),
        include_names: Default::default(),
    });

    resolve.packages[union_package]
        .worlds
        .insert("union".into(), union_world);

    let mut clone_maps = CloneMaps::default();
    for &world in &worlds {
        resolve.merge_worlds(world, union_world, &mut clone_maps)?;
    }

    let (mut bindings, metadata) = wit_dylib::create_with_metadata(
        &resolve,
        union_world,
        Some(&mut DylibOpts {
            interpreter: Some("libcomponentize_py_runtime.so".into()),
            async_: Default::default(),
        }),
    );

    CustomSection {
        name: Cow::Borrowed("component-type:componentize-py-union"),
        data: Cow::Owned(metadata::encode(
            &resolve,
            union_world,
            wit_component::StringEncoding::UTF8,
            None,
        )?),
    }
    .append_to(&mut bindings);

    let imported_function_indexes = metadata
        .import_funcs
        .iter()
        .enumerate()
        .map(|(index, func)| ((func.interface.as_deref(), func.name.as_str()), index))
        .collect();

    let exported_function_indexes = metadata
        .export_funcs
        .iter()
        .enumerate()
        .map(|(index, func)| ((func.interface.as_deref(), func.name.as_str()), index))
        .collect();

    let mut reverse_cloned_types = HashMap::new();
    for (&original, &clone) in clone_maps.types() {
        assert!(reverse_cloned_types.insert(clone, original).is_none());
    }

    let original = |ty| {
        if let Some(&original) = reverse_cloned_types.get(&ty) {
            original
        } else {
            ty
        }
    };

    let stream_and_future_indexes = metadata
        .streams
        .iter()
        .enumerate()
        .map(|(index, stream)| (original(stream.id), index))
        .chain(
            metadata
                .futures
                .iter()
                .enumerate()
                .map(|(index, future)| (original(future.id), index)),
        )
        .collect();

    let summary = Summary::try_new(
        &resolve,
        &worlds,
        &import_interface_names,
        &export_interface_names,
        &imported_function_indexes,
        &exported_function_indexes,
        &stream_and_future_indexes,
    )?;

    let need_async = summary.need_async();

    // Now that we know whether to use the sync or async version of
    // `libcomponentize_py_runtime.so`, update `libraries` accordingly.
    //
    // Note that we have two separate versions because older runtimes don't
    // understand the new async ABI, so we only use the async version if it's
    // actually needed.
    let mut libraries = libraries
        .into_iter()
        .filter_map(|library| match (need_async, library.name.as_str()) {
            (true, "libcomponentize_py_runtime_sync.so")
            | (false, "libcomponentize_py_runtime_async.so") => None,
            (true, "libcomponentize_py_runtime_async.so")
            | (false, "libcomponentize_py_runtime_sync.so") => Some(Library {
                name: "libcomponentize_py_runtime.so".into(),
                ..library
            }),
            _ => Some(library),
        })
        .collect::<Vec<_>>();

    libraries.push(Library {
        name: "libcomponentize_py_bindings.so".into(),
        module: bindings,
        dl_openable: false,
    });

    let component = link::link_libraries(&libraries)?;

    let stubbed_component = if stub_wasi {
        stubwasi::link_stub_modules(libraries)?
    } else {
        None
    };

    // Pre-initialize the component by running it through
    // `component_init_transform::initialize`.  Currently, this is the
    // application's first and only chance to load any standard or third-party
    // modules since we do not yet include a virtual filesystem in the component
    // to make those modules available at runtime.

    let stdout = MemoryOutputPipe::new(10000);
    let stderr = MemoryOutputPipe::new(10000);

    let mut wasi = WasiCtxBuilder::new();
    wasi.stdin(MemoryInputPipe::new(Bytes::new()))
        .stdout(stdout.clone())
        .stderr(stderr.clone())
        .env("PYTHONUNBUFFERED", "1")
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

    // For each Python package with a `componentize-py.toml` file that specifies
    // where generated bindings for that package should be placed, generate the
    // bindings and place them as indicated.

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

    // If the caller specified a world and we haven't already generated bindings
    // for it above, do so now.
    if let (Some(world), false) = (main_world, saw_main_world) {
        let module = world_module.unwrap_or(DEFAULT_WORLD_MODULE);
        let world_dir = tempfile::tempdir()?;
        let module_path = world_dir.path().join(module);
        fs::create_dir_all(&module_path)?;
        summary.generate_code(&module_path, world, module, &mut locations, false)?;
        world_dir_mounts.push((vec!["world".to_owned()], world_dir));

        // The helper utilities are hard-coded to assume the world module is
        // named `wit_world`.  Here we replace that with the actual world module
        // name.
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
            if DEBUG_PYTHON_BINDINGS {
                eprintln!("world dir path: {}", world_dir.path().display());
            }
            wasi.preopened_dir(world_dir.path(), mount, DirPerms::all(), FilePerms::all())?;
        }
    }

    if DEBUG_PYTHON_BINDINGS {
        // Prevent temporary directories from being deleted:
        std::mem::forget(world_dir_mounts);
    }

    // Generate a `Symbols` object containing metadata to be passed to the
    // pre-init function.  The runtime library will use this to look up types
    // and functions that will later be referenced by the generated Wasm code.
    let symbols = summary.collect_symbols(&locations, &metadata, &clone_maps);

    // Finally, pre-initialize the component, writing the result to
    // `output_path`.

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
    config.wasm_component_model_async(true);
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
    paths: &[impl AsRef<Path>],
    world: Option<&str>,
    features: &[String],
    all_features: bool,
) -> Result<(Resolve, WorldId)> {
    // If no WIT directory was provided as a parameter and none were referenced
    // by Python packages, use ./wit by default.
    if paths.is_empty() {
        let paths = &[Path::new("wit")];
        return parse_wit(paths, world, features, all_features);
    }
    debug_assert!(!paths.is_empty(), "The paths should not be empty");

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

    let mut last_pkg = None;
    for path in paths.iter().map(AsRef::as_ref) {
        let pkg = if path.is_dir() {
            resolve.push_dir(path)?.0
        } else {
            let pkg = UnresolvedPackageGroup::parse_file(path)?;
            resolve.push_group(pkg)?
        };
        last_pkg = Some(pkg);
    }

    let pkg = last_pkg.unwrap(); // The paths should not be empty
    let world = resolve.select_world(&[pkg], world)?;

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
            // Note that we do _not_ stub interfaces which appear to be part of
            // WASIp2 since those should be provided by the
            // `wasmtime_wasi::add_to_linker_async` call above, and adding stubs
            // to those same interfaces would just cause trouble.
            if !is_wasip2_cli(&interface_name)
                && let Ok(mut instance) = linker.instance(&interface_name)
            {
                for stub in stubs {
                    let interface_name = interface_name.clone();
                    match stub {
                        Stub::Function(name) => instance.func_new(name, {
                            let name = name.clone();
                            move |_, _, _, _| {
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
                        move |_, _, _, _| Err(anyhow!("called trapping stub: {name}"))
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

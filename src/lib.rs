#![deny(warnings)]

use {
    anyhow::{Context, Error, Result, anyhow, bail, ensure},
    async_trait::async_trait,
    bytes::Bytes,
    component_init_transform::Invoker,
    futures::future::FutureExt,
    indexmap::{IndexMap, IndexSet},
    serde::Deserialize,
    std::{
        borrow::Cow,
        collections::{HashMap, HashSet},
        fs,
        io::Cursor,
        iter,
        ops::Deref,
        path::{Path, PathBuf},
        str,
    },
    summary::{Locations, Summary},
    tar::Archive,
    wasm_encoder::{CustomSection, Section as _},
    wasmtime::{
        Config, Engine, Store,
        component::{Component, Instance, Linker, ResourceTable, ResourceType},
    },
    wasmtime_wasi::{
        FsPerms, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView,
        p2::pipe::{MemoryInputPipe, MemoryOutputPipe},
    },
    wit_component::metadata,
    wit_dylib::DylibOpts,
    wit_parser::{
        CloneMaps, FunctionKind, Package, PackageId, PackageName, Resolve, Stability, TypeDefKind,
        World, WorldId, WorldItem, WorldKey,
    },
    zstd::Decoder,
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

/// Default bindings module name, overridable via `--bindings-module`.
static DEFAULT_BINDINGS_MODULE: &str = "wit";

wasmtime::component::bindgen!({
    path: "wit",
    world: "init",
    exports: { default: async },
});

pub struct Ctx {
    wasi: WasiCtx,
    table: ResourceTable,
}

impl WasiView for Ctx {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

pub struct Library {
    name: String,
    module: Vec<u8>,
    dl_openable: bool,
}

#[derive(Deserialize)]
struct RawComponentizePyConfig {
    bindings: Option<String>,
    wit_directory: Option<String>,
    #[serde(default)]
    import_interface_names: HashMap<String, String>,
    #[serde(default)]
    export_interface_names: HashMap<String, String>,
    full_names: Option<bool>,
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
        warn_full_names_deprecated(raw.full_names, Some(path));

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
        Ok(func.call_async(&mut self.store, ()).await?.0)
    }

    async fn call_s64(&mut self, function: &str) -> Result<i64> {
        let func = self
            .instance
            .get_typed_func::<(), (i64,)>(&mut self.store, function)?;
        Ok(func.call_async(&mut self.store, ()).await?.0)
    }

    async fn call_f32(&mut self, function: &str) -> Result<f32> {
        let func = self
            .instance
            .get_typed_func::<(), (f32,)>(&mut self.store, function)?;
        Ok(func.call_async(&mut self.store, ()).await?.0)
    }

    async fn call_f64(&mut self, function: &str) -> Result<f64> {
        let func = self
            .instance
            .get_typed_func::<(), (f64,)>(&mut self.store, function)?;
        Ok(func.call_async(&mut self.store, ()).await?.0)
    }

    async fn call_list_u8(&mut self, function: &str) -> Result<Vec<u8>> {
        let func = self
            .instance
            .get_typed_func::<(), (Vec<u8>,)>(&mut self.store, function)?;
        Ok(func.call_async(&mut self.store, ()).await?.0)
    }
}

pub struct BindingsGenerator<'a> {
    pub wit_paths: &'a [&'a Path],
    pub worlds: &'a [&'a str],
    pub features: &'a [&'a str],
    pub all_features: bool,
    pub bindings_module: Option<&'a str>,
    pub output_dir: &'a Path,
    pub import_interface_names: &'a HashMap<&'a str, &'a str>,
    pub export_interface_names: &'a HashMap<&'a str, &'a str>,
}

impl BindingsGenerator<'_> {
    pub fn generate(&self) -> Result<()> {
        // Here we parse the specified WIT paths and resolve the specified
        // worlds, then union them together and generate stub code for the
        // unioned world.
        //
        // Note that this code is not meant to be run; every import raises a
        // `NotImplementedError`.  This is only meant for use by e.g. IDEs, type
        // checkers, and document generators.  The real code (i.e. the code
        // which is actually hooked up to real component imports and exports)
        // will be generated on-the-fly by `ComponentGenerator::componentize`.
        //
        // Note that, unlike in `ComponentGenerator::componentize`, we make no
        // attempt to discover or use `componentize-py.toml` files here; we only
        // use the paths in `self.wit_paths` and only output the generated code
        // to a single directory.

        let mut resolve = Resolve {
            all_features: self.all_features,
            features: parse_features(self.features),
            ..Default::default()
        };

        let mut packages = Vec::new();
        for &path in self.wit_paths {
            packages.push((path, resolve.push_path(path)?.0));
        }

        if packages.is_empty() {
            // If no WIT directory was provided as a parameter, use ./wit by default.
            packages.push((Path::new("wit"), resolve.push_path("wit")?.0));
        }

        let worlds = select_worlds(&resolve, self.worlds, &packages)?;
        let world = match &worlds.iter().copied().collect::<Vec<_>>()[..] {
            [] => select_world(&resolve, None, &packages)?,
            &[world] => world,
            worlds => union_world(&mut resolve, "union", worlds, &mut CloneMaps::default())?,
        };

        let import_function_indexes = &HashMap::new();
        let export_function_indexes = &HashMap::new();
        let stream_and_future_indexes = &HashMap::new();
        let summary = Summary::try_new(
            &resolve,
            &iter::once(world).collect(),
            self.import_interface_names,
            self.export_interface_names,
            import_function_indexes,
            export_function_indexes,
            stream_and_future_indexes,
        )?;
        let bindings_module = self.bindings_module.unwrap_or(DEFAULT_BINDINGS_MODULE);
        validate_bindings_module(bindings_module)?;
        let world_dir = self.output_dir.join(bindings_module.replace('.', "/"));
        // A `wit/` WIT-source directory clashes with the default module name.
        if world_dir.is_dir() {
            if contains_wit_files(&world_dir)? {
                bail!(
                    "refusing to write bindings into {}, which contains WIT source files; \
                     specify a different output directory or `--bindings-module`",
                    world_dir.display()
                );
            }
            fs::remove_dir_all(&world_dir)?;
        }
        fs::create_dir_all(&world_dir)?;
        create_module_ancestors(self.output_dir, bindings_module)?;
        let mut locations = Locations::default();
        summary.generate_code(&world_dir, world, bindings_module, &mut locations, true)?;
        let helper_module_paths = summary.helper_module_paths(&locations);

        let mut archive = Archive::new(Decoder::new(Cursor::new(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/bundled.tar.zst"
        ))))?);

        // Leave anything else in the output dir alone (e.g. the app itself)
        for entry in archive.entries()? {
            let mut entry = entry?;
            let relative = entry.path()?.into_owned();
            ensure!(
                relative
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_))),
                "invalid path in bundled archive: {}",
                relative.display()
            );
            let path = self.output_dir.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            entry.unpack(&path)?;
            if path.is_file() {
                resolve_helper_placeholders(&path, &helper_module_paths)?;
            }
        }

        Ok(())
    }
}

/// Warn about the deprecated `full_names` option (CLI, TOML, or Python API).
pub(crate) fn warn_full_names_deprecated(full_names: Option<bool>, source: Option<&Path>) {
    if full_names == Some(true) {
        let source = source
            .map(|path| format!(" in {}", path.display()))
            .unwrap_or_default();
        eprintln!(
            "warning: `full_names`{source} is deprecated and has no effect; fully-qualified \
             module names are always used"
        );
    }
}

/// Resolve the deprecated `world_module` spelling of `bindings_module`, warning
/// about it and about `full_names`.
pub(crate) fn resolve_deprecated(
    bindings_module: Option<String>,
    world_module: Option<String>,
    full_names: Option<bool>,
    quiet: bool,
) -> Result<Option<String>> {
    if !quiet {
        warn_full_names_deprecated(full_names, None);
        if world_module.is_some() {
            eprintln!("warning: `world_module` is deprecated; use `bindings_module`");
        }
    }
    ensure!(
        !(bindings_module.is_some() && world_module.is_some()),
        "`bindings_module` and its deprecated spelling `world_module` are mutually exclusive"
    );

    Ok(bindings_module.or(world_module))
}

fn contains_wit_files(dir: &Path) -> Result<bool> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            if contains_wit_files(&path)? {
                return Ok(true);
            }
        } else if path.extension().is_some_and(|ext| ext == "wit") {
            return Ok(true);
        }
    }

    Ok(false)
}

/// Reject `--bindings-module` values which are not importable Python paths.
fn validate_bindings_module(module: &str) -> Result<()> {
    if module
        .split('.')
        .all(|c| summary::is_python_identifier(c) && !summary::is_python_keyword(c))
    {
        Ok(())
    } else {
        bail!("`{module}` is not a valid Python module path")
    }
}

/// Make each ancestor of a dotted bindings module a regular package.
fn create_module_ancestors(root: &Path, module: &str) -> Result<()> {
    let components = module.split('.').collect::<Vec<_>>();
    let mut dir = root.to_owned();
    for component in &components[..components.len() - 1] {
        dir = dir.join(component);
        fs::create_dir_all(&dir)?;
        let init = dir.join("__init__.py");
        if !init.exists() {
            fs::write(init, "")?;
        }
    }

    Ok(())
}

/// Replace each helper placeholder with the module path generated for it; an
/// unimported interface has no entry and keeps its placeholder.
fn resolve_helper_placeholders(path: &Path, replacements: &[(&str, String)]) -> Result<()> {
    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            resolve_helper_placeholders(&entry?.path(), replacements)?;
        }
    } else {
        // Skip binary files, and leave files without a placeholder untouched.
        let bytes = fs::read(path)?;
        if let Ok(text) = str::from_utf8(&bytes)
            && let Some(replaced) = replacements
                .iter()
                .filter(|(placeholder, _)| text.contains(placeholder))
                .fold(None, |replaced: Option<String>, (placeholder, module)| {
                    Some(
                        replaced
                            .unwrap_or_else(|| text.to_owned())
                            .replace(placeholder, module),
                    )
                })
        {
            fs::write(path, replaced)?;
        }
    }

    Ok(())
}

pub type AddToLinker<'a> = Option<&'a dyn Fn(&mut Linker<Ctx>) -> Result<()>>;

pub struct ComponentGenerator<'a> {
    pub wit_paths: &'a [&'a Path],
    pub worlds: &'a [&'a str],
    pub features: &'a [&'a str],
    pub all_features: bool,
    pub bindings_module: Option<&'a str>,
    pub python_path: &'a [&'a str],
    pub module_worlds: &'a [(&'a str, &'a [&'a str])],
    pub app_name: &'a str,
    pub output_path: &'a Path,
    pub add_to_linker: AddToLinker<'a>,
    pub stub_wasi: bool,
    pub import_interface_names: &'a HashMap<&'a str, &'a str>,
    pub export_interface_names: &'a HashMap<&'a str, &'a str>,
    pub intersect_world: Option<&'a str>,
}

impl ComponentGenerator<'_> {
    pub async fn generate(&self) -> Result<()> {
        // This is a _big_ function.  Here's an overview of what it does.
        //
        // Our goal here is to collect one or more WIT paths, parse them, and
        // resolve one or more WIT worlds, then generate a component that
        // targets the union of those worlds.
        //
        // What complicates this process is that some of the WIT paths my have
        // been specified explicitly via `self.wit_paths`, but some of them will
        // be implicitly added as we traverse `self.python_path` and discover
        // any `componentize-py.toml` files.  A Python module containing such a
        // file may use it specify its own WIT path as well is where to place
        // the generated code for any worlds which have been resolved in that
        // WIT path.  In this case we say the Python module "covers" or "owns"
        // those worlds. Therefore, while the output component will target the
        // union of all the worlds, the code generated for that unioned world
        // may be spread across multiple modules according to which modules
        // "cover" which parts of the unioned world.
        //
        // Note that, when a given Python module "covers" a set of worlds, it
        // may contain its own, pregenerated bindings for use by IDEs,
        // type-checkers, and document generators.  Those bindings will be
        // ignored here and replaced by code we generate on-the-fly and which is
        // hooked up to the native Wasm imports and exports we'll be
        // synthesizing.
        //
        // In a nutshell, our job here is to not only create a component which
        // targets the union of the specified worlds, but to generate code and
        // place it into one or more modules as directed by the configuration
        // settings specified in `self` and any `componentize-py.toml` files
        // discovered.
        //
        // Once we've done that, there's one final step: pre-initialize the
        // component.  This involves running the top level script specified by
        // `self.app_name` in a controled environment where it has access to all
        // the directories in `self.python_path` plus directories containing any
        // generated code produced earlier.  Assuming this step succeeds, we'll
        // snapshot the result and emit the snapshot as the final output.

        // Remove non-existent elements from `python_path` so we don't choke on them
        // later:
        let python_path = &self
            .python_path
            .iter()
            .filter_map(|&s| Path::new(s).exists().then_some(s))
            .collect::<Vec<_>>();

        let embedded_python_standard_lib = prelink::embedded_python_standard_library()?;
        let embedded_helper_utils = prelink::embedded_helper_utils()?;

        let (configs, libraries) = prelink::search_for_libraries_and_configs(
            python_path,
            self.module_worlds,
            self.worlds,
        )?;

        let mut union_number = 0;
        let mut next_union_name = move || {
            let name = format!("union{union_number}");
            union_number += 1;
            name
        };

        let import_interface_names = self
            .import_interface_names
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

        let export_interface_names = self
            .export_interface_names
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

        let features = parse_features(self.features);

        let mut resolve = Resolve {
            all_features: self.all_features,
            features: features.clone(),
            ..Default::default()
        };

        let mut packages = Vec::new();

        let configs = configs
            .iter()
            .map(|(module, &(ref config, worlds))| {
                Ok((
                    module,
                    if let Some(path) = config.config.wit_directory.as_deref() {
                        // The list of worlds we have here includes all the
                        // worlds which might possibly be covered by this
                        // particular Python module, but not all of them
                        // necessarily _are_ covered by it.  Here we filter the
                        // list by creating a `Resolve` which _only_ includes
                        // the path specified in this module's
                        // `componentize-py.toml` file and looking up the world
                        // there.  If there's a match, we'll keep it; otherwise,
                        // we toss it out and assume it will be (or has been)
                        // covered elsewhere.

                        let mut tmp = Resolve {
                            all_features: self.all_features,
                            features: features.clone(),
                            ..Default::default()
                        };

                        let package = tmp.push_path(path)?.0;

                        let worlds = worlds
                            .iter()
                            .filter_map(|world| {
                                select_world(&tmp, Some(world), &[(path, package)]).ok()
                            })
                            .collect::<Vec<_>>();

                        let remap = resolve.merge(tmp)?;

                        packages.push((path, remap.packages[package.index()]));

                        let worlds = worlds
                            .into_iter()
                            .map(|v| remap.worlds[v.index()].unwrap())
                            .collect();

                        (config, worlds)
                    } else {
                        (config, Vec::new())
                    },
                ))
            })
            .collect::<Result<IndexMap<_, _>>>()?;

        for path in self.wit_paths {
            packages.push((path, resolve.push_path(path)?.0));
        }

        if packages.is_empty() {
            // If no WIT directory was provided as a parameter and none were
            // referenced by Python packages, use ./wit by default.
            packages.push((Path::new("wit"), resolve.push_path("wit")?.0));
        }

        let worlds = select_worlds(&resolve, self.worlds, &packages)?;

        let intersector = if let Some(world) = self.intersect_world {
            let intersector = select_world(&resolve, Some(world), &packages)?;

            for &world in &worlds {
                intersect_world(&mut resolve, intersector, world);
            }

            for (_, worlds) in configs.values() {
                for &world in worlds {
                    intersect_world(&mut resolve, intersector, world);
                }
            }

            Some(intersector)
        } else {
            None
        };

        let mut all_worlds = worlds
            .iter()
            .copied()
            .chain(
                configs
                    .values()
                    .flat_map(|(_, worlds)| worlds.iter().copied()),
            )
            .collect::<IndexSet<_>>();

        if all_worlds.is_empty() {
            // No worlds specified; pick the default one, if available:
            let world = select_world(&resolve, None, &packages)?;

            if let Some(intersector) = intersector {
                intersect_world(&mut resolve, intersector, world);
            }

            all_worlds.insert(world);
        }

        // Now that we've parsed all known WIT files and resolved all relevant
        // worlds, we collect them all into a single world, unioning them
        // together if there's more than one.
        //
        // This unified world represents the target world of the component we're
        // creating, but note that, because parts of that world may be covered
        // by dependencies, we may need to split code generation across several
        // Python modules, so we still need to keep track of the original list
        // of worlds and which modules they belong to.

        let mut clone_maps = CloneMaps::default();

        let mut unioned = |resolve: &mut _, worlds: &[_]| {
            anyhow::Ok(match worlds {
                [] => None,
                &[world] => Some(world),
                worlds => Some(union_world(
                    resolve,
                    &next_union_name(),
                    worlds,
                    &mut clone_maps,
                )?),
            })
        };

        let world = unioned(
            &mut resolve,
            &all_worlds.iter().copied().collect::<Vec<_>>(),
        )?
        .unwrap();

        // Determine which worlds are covered by which Python modules and, for
        // each module, union the ones covered by the module into a single
        // world.

        let mut worlds_to_generate = all_worlds.clone();

        let configs = configs
            .iter()
            .map(|(module, (config, worlds))| {
                // If a `bindings` config is specified for this module, we will
                // generate code for these worlds separately, so remove them
                // from `worlds_to_generate`.
                if config.config.bindings.is_some() {
                    worlds_to_generate = worlds_to_generate
                        .difference(&worlds.iter().copied().collect::<IndexSet<_>>())
                        .copied()
                        .collect();
                }

                let world = unioned(&mut resolve, worlds)?;

                Ok((module, (config, world)))
            })
            .collect::<Result<IndexMap<_, _>>>()?;

        // Here we union together any worlds not covered by any of the Python
        // modules above.  We'll generate code for this world in its own,
        // synthesized module.

        let world_to_generate = unioned(
            &mut resolve,
            &worlds_to_generate.into_iter().collect::<Vec<_>>(),
        )?;

        // Extract relevant metadata from the `Resolve` into a `Summary` instance,
        // which we'll use to generate Wasm- and Python-level bindings.

        let (mut bindings, metadata) = wit_dylib::create_with_metadata(
            &resolve,
            world,
            Some(&mut DylibOpts {
                stack_pointer: wit_dylib::StackPointer::Global,
                interpreter: Some("libcomponentize_py_runtime.so".into()),
                async_: Default::default(),
            }),
        );

        CustomSection {
            name: Cow::Borrowed("component-type:componentize-py-union"),
            data: Cow::Owned(metadata::encode(
                &resolve,
                world,
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
            &all_worlds,
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

        let stubbed_component = if self.stub_wasi {
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
                FsPerms::ReadWrite,
            )?
            .preopened_dir(embedded_helper_utils.path(), "bundled", FsPerms::ReadWrite)?;

        // Generate guest mounts for each host directory in `python_path`.
        for (index, path) in python_path.iter().enumerate() {
            wasi.preopened_dir(path, index.to_string(), FsPerms::ReadWrite)?;
        }

        // For each Python module with a `componentize-py.toml` file that
        // specifies where generated bindings for that package should be placed,
        // generate the bindings and place them as indicated.

        let mut world_dir_mounts = Vec::new();
        let mut locations = Locations::default();

        for (config, world, binding_path) in configs.values().filter_map(|&(ref config, world)| {
            Some((config, world?, config.config.bindings.as_deref()?))
        }) {
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
            validate_bindings_module(&binding_module)
                .with_context(|| format!("in `bindings` for {}", config.path.display()))?;

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

        // Here we generate code for any worlds not covered by any of the Python
        // modules we visited above.
        let module = self.bindings_module.unwrap_or(DEFAULT_BINDINGS_MODULE);
        validate_bindings_module(module)?;
        if let Some(world) = world_to_generate {
            let world_dir = tempfile::tempdir()?;
            let module_path = world_dir.path().join(module.replace('.', "/"));
            fs::create_dir_all(&module_path)?;
            create_module_ancestors(world_dir.path(), module)?;
            summary.generate_code(&module_path, world, module, &mut locations, false)?;
            world_dir_mounts.push((vec!["world".to_owned()], world_dir));
        };

        // One shared mount, but each interface has exactly one owning module.
        resolve_helper_placeholders(
            embedded_helper_utils.path(),
            &summary.helper_module_paths(&locations),
        )?;

        for (mounts, world_dir) in world_dir_mounts.iter() {
            for mount in mounts {
                if DEBUG_PYTHON_BINDINGS {
                    eprintln!(
                        "world dir path: {}; mount: {mount}",
                        world_dir.path().display()
                    );
                }
                wasi.preopened_dir(world_dir.path(), mount, FsPerms::ReadWrite)?;
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
        // `self.output_path`.

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
        config.wasm_component_model_map(true);

        let engine = Engine::new(&config)?;

        let mut linker = Linker::new(&engine);
        let added_to_linker = if let Some(add_to_linker) = self.add_to_linker {
            add_to_linker(&mut linker)?;
            true
        } else {
            false
        };

        let mut store = Store::new(&engine, Ctx { wasi, table });

        let stub_wasi = self.stub_wasi;
        let app_name = self.app_name.to_owned();
        let component = component_init_transform::initialize_staged(
            &component,
            stubbed_component
                .as_ref()
                .map(|(component, map)| (component.deref(), map as &dyn Fn(u32) -> u32)),
            move |instrumented| {
                async move {
                    let component = &Component::new(&engine, instrumented)?;
                    if !added_to_linker {
                        add_wasi_and_stubs(&resolve, &all_worlds, &mut linker)?;
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

        // Checks if the output directory exists, and creates it if it doesn't.
        if let Some(parent) = self.output_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(self.output_path, component)?;

        Ok(())
    }
}

fn parse_features(features: &[&str]) -> IndexSet<String> {
    features
        .iter()
        .flat_map(|features| {
            features
                .split(',')
                .flat_map(|s| s.split_whitespace())
                .filter(|f| !f.is_empty())
        })
        .map(String::from)
        .collect()
}

fn available_worlds(resolve: &Resolve) -> Vec<String> {
    resolve
        .worlds
        .iter()
        .map(|(_, world)| {
            if let Some(package) = world.package {
                let package = &resolve.packages[package].name;
                let version = if let Some(version) = &package.version {
                    format!("@{version}")
                } else {
                    String::new()
                };
                let package_namespace = &package.namespace;
                let package_name = &package.name;
                let name = &world.name;
                format!("{package_namespace}:{package_name}/{name}{version}")
            } else {
                world.name.clone()
            }
        })
        .collect()
}

fn select_world(
    resolve: &Resolve,
    world: Option<&str>,
    packages: &[(&Path, PackageId)],
) -> Result<WorldId> {
    // First, try looking in the top-level packages
    resolve
        .select_world(&packages.iter().map(|&(_, v)| v).collect::<Vec<_>>(), world)
        .or_else(|_| {
            // That didn't work; now try _all_ known packages
            resolve
                .select_world(
                    &resolve.packages.iter().map(|(v, _)| v).collect::<Vec<_>>(),
                    world,
                )
                .with_context(|| {
                    let worlds = available_worlds(resolve);
                    let paths = packages.iter().map(|&(v, _)| v).collect::<Vec<_>>();
                    format!(
                        "Unable to resolve `{world:?}`.\n\
                         Available worlds: {worlds:#?}\n\
                         WIT paths: {paths:#?}"
                    )
                })
        })
}

fn select_worlds(
    resolve: &Resolve,
    worlds: &[&str],
    packages: &[(&Path, PackageId)],
) -> Result<IndexSet<WorldId>> {
    worlds
        .iter()
        .map(|world| select_world(resolve, Some(world), packages))
        .collect()
}

fn union_world(
    resolve: &mut Resolve,
    name: &str,
    worlds: &[WorldId],
    clone_maps: &mut CloneMaps,
) -> Result<WorldId> {
    let union_package = resolve.packages.alloc(Package {
        name: PackageName {
            namespace: "componentize-py".into(),
            name: name.into(),
            version: None,
        },
        docs: Default::default(),
        interfaces: Default::default(),
        worlds: Default::default(),
    });

    let union_world = resolve.worlds.alloc(World {
        name: name.into(),
        imports: Default::default(),
        exports: Default::default(),
        package: Some(union_package),
        docs: Default::default(),
        stability: Stability::Unknown,
        includes: Default::default(),
        span: Default::default(),
    });

    resolve.packages[union_package]
        .worlds
        .insert(name.into(), union_world);

    for &world in worlds {
        resolve.merge_worlds(world, union_world, clone_maps)?;
    }

    Ok(union_world)
}

fn intersect_world(resolve: &mut Resolve, intersector: WorldId, world: WorldId) {
    let (imports_to_remove, exports_to_remove) = {
        let intersector = &resolve.worlds[intersector];
        let world = &resolve.worlds[world];

        // Note that we make no effort to check whether the world items
        // corresponding to the same keys found in `intersector` and `world`
        // actually match.  We only care about the keys in `intersector` and ignore
        // the items they map to.

        (
            world
                .imports
                .keys()
                .filter_map(|name| {
                    if intersector.imports.contains_key(name) {
                        None
                    } else {
                        Some(name.clone())
                    }
                })
                .collect::<HashSet<_>>(),
            world
                .exports
                .keys()
                .filter_map(|name| {
                    if intersector.exports.contains_key(name) {
                        None
                    } else {
                        Some(name.clone())
                    }
                })
                .collect::<HashSet<_>>(),
        )
    };

    let world = &mut resolve.worlds[world];
    world.imports.retain(|k, _| !imports_to_remove.contains(k));
    world.exports.retain(|k, _| !exports_to_remove.contains(k));
}

fn add_wasi_and_stubs(
    resolve: &Resolve,
    worlds: &IndexSet<WorldId>,
    linker: &mut Linker<Ctx>,
) -> Result<()> {
    wasmtime_wasi::p2::add_to_linker_async(linker)?;

    enum Stub<'a> {
        Function(&'a String, &'a FunctionKind),
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
                    for (function_name, function) in &interface.functions {
                        stubs
                            .entry(Some(interface_name.clone()))
                            .or_default()
                            .push(Stub::Function(function_name, &function.kind));
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
                        .push(Stub::Function(&function.name, &function.kind));
                }
                WorldItem::Type { id, .. } => {
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
                        Stub::Function(name, kind) => {
                            if kind.is_async() {
                                instance.func_new_concurrent(name, {
                                    let name = name.clone();
                                    move |_, _, _, _| {
                                        let interface_name = interface_name.clone();
                                        let name = name.clone();
                                        Box::pin(async move {
                                            Err(wasmtime::format_err!(
                                                "called trapping stub: {interface_name}#{name}"
                                            ))
                                        })
                                    }
                                })
                            } else {
                                instance.func_new(name, {
                                    let name = name.clone();
                                    move |_, _, _, _| {
                                        Err(wasmtime::format_err!(
                                            "called trapping stub: {interface_name}#{name}"
                                        ))
                                    }
                                })
                            }
                        }
                        Stub::Resource(name) => instance
                            .resource(name, ResourceType::host::<()>(), {
                                let name = name.clone();
                                move |_, _| {
                                    Err(wasmtime::format_err!(
                                        "called trapping stub: {interface_name}#{name}"
                                    ))
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
                    Stub::Function(name, kind) => {
                        if kind.is_async() {
                            instance.func_new_concurrent(name, {
                                let name = name.clone();
                                move |_, _, _, _| {
                                    let name = name.clone();
                                    Box::pin(async move {
                                        Err(wasmtime::format_err!("called trapping stub: {name}"))
                                    })
                                }
                            })
                        } else {
                            instance.func_new(name, {
                                let name = name.clone();
                                move |_, _, _, _| {
                                    Err(wasmtime::format_err!("called trapping stub: {name}"))
                                }
                            })
                        }
                    }
                    Stub::Resource(name) => instance
                        .resource(name, ResourceType::host::<()>(), {
                            let name = name.clone();
                            move |_, _| Err(wasmtime::format_err!("called trapping stub: {name}"))
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

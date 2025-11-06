#![deny(warnings)]

use std::{
    collections::{HashMap, HashSet},
    fs::{self},
    io::Cursor,
    ops::Deref,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use indexmap::IndexMap;
use tar::Archive;
use tempfile::TempDir;
use zstd::Decoder;

use crate::{ComponentizePyConfig, ConfigContext, Library, RawComponentizePyConfig};

static NATIVE_EXTENSION_SUFFIX: &str = ".cpython-314-wasm32-wasi.so";

type ConfigsMatchedWorlds<'a> =
    IndexMap<String, (ConfigContext<ComponentizePyConfig>, Option<&'a str>)>;

pub fn embedded_python_standard_library() -> Result<TempDir> {
    // Untar the embedded copy of the Python standard library into a temporary
    // directory
    let stdlib = tempfile::tempdir()?;

    Archive::new(Decoder::new(Cursor::new(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/python-lib.tar.zst"
    ))))?)
    .unpack(stdlib.path())
    .unwrap();

    Ok(stdlib)
}

pub fn embedded_helper_utils() -> Result<TempDir> {
    // Untar the embedded copy of helper utilities into a temporary directory
    let bundled = tempfile::tempdir()?;

    Archive::new(Decoder::new(Cursor::new(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/bundled.tar.zst"
    ))))?)
    .unpack(bundled.path())
    .unwrap();

    Ok(bundled)
}

pub fn bundle_libraries(library_path: Vec<(&str, Vec<PathBuf>)>) -> Result<Vec<Library>> {
    let mut libraries = vec![
        Library {
            name: "libcomponentize_py_runtime_sync.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libcomponentize_py_runtime_sync.so.zst"
            ))))?,
            dl_openable: false,
        },
        Library {
            name: "libcomponentize_py_runtime_async.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libcomponentize_py_runtime_async.so.zst"
            ))))?,
            dl_openable: false,
        },
        Library {
            name: "libpython3.14.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libpython3.14.so.zst"
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
    ];

    for (index, (path, libs)) in library_path.iter().enumerate() {
        for library in libs {
            let path = library
                .strip_prefix(path)
                .unwrap()
                .to_str()
                .context("non-UTF-8 path")
                .unwrap()
                .replace('\\', "/");

            libraries.push(Library {
                name: format!("/{index}/{path}"),
                module: fs::read(library).with_context(|| library.display().to_string())?,
                dl_openable: true,
            });
        }
    }

    Ok(libraries)
}

pub fn search_for_libraries_and_configs<'a>(
    python_path: &'a Vec<&'a str>,
    module_worlds: &'a [(&'a str, &'a str)],
    world: Option<&'a str>,
) -> Result<(ConfigsMatchedWorlds<'a>, Vec<Library>)> {
    let mut raw_configs: Vec<ConfigContext<RawComponentizePyConfig>> = Vec::new();
    let mut library_path: Vec<(&str, Vec<PathBuf>)> = Vec::with_capacity(python_path.len());
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

    let libraries = bundle_libraries(library_path)?;

    // Validate the paths parsed from any componentize-py.toml files discovered
    // above and match them up with `module_worlds` entries.  Note that we use
    // an `IndexMap` to preserve the order specified in `module_worlds`, which
    // is required to be topologically sorted with respect to package
    // dependencies.
    //
    // For any packages which contain componentize-py.toml files but no
    // corresponding `module_worlds` entry, we use the `world` parameter as a
    // default.
    let configs: IndexMap<String, (ConfigContext<ComponentizePyConfig>, Option<&str>)> = {
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

    Ok((configs, libraries))
}

fn search_directory(
    root: &Path,
    path: &Path,
    libraries: &mut Vec<PathBuf>,
    configs: &mut Vec<ConfigContext<RawComponentizePyConfig>>,
    modules_seen: &mut HashSet<String>,
) -> Result<()> {
    if path.is_dir() {
        for entry in fs::read_dir(path).with_context(|| path.display().to_string())? {
            search_directory(root, &entry?.path(), libraries, configs, modules_seen)?;
        }
    } else if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
        if name.ends_with(NATIVE_EXTENSION_SUFFIX) {
            libraries.push(path.to_owned());
        } else if name == "componentize-py.toml" {
            let root = root
                .canonicalize()
                .with_context(|| root.display().to_string())?;
            let path = path
                .canonicalize()
                .with_context(|| path.display().to_string())?;

            let module = module_name(&root, &path)
                .ok_or_else(|| anyhow!("unable to determine module name for {}", path.display()))?;

            let mut push = true;
            for existing in &mut *configs {
                if path == existing.path.join("componentize-py.toml") {
                    // When one directory in `PYTHON_PATH` is a subdirectory of
                    // the other, we consider the subdirectory to be the true
                    // owner of the file.  This is important later, when we
                    // derive a package name by stripping the root directory
                    // from the file path.
                    if root > existing.root {
                        module.clone_into(&mut existing.module);
                        root.clone_into(&mut existing.root);
                        path.parent().unwrap().clone_into(&mut existing.path);
                    }
                    push = false;
                    break;
                } else {
                    // If we find a componentize-py.toml file under a Python
                    // module which will not be used because we already found a
                    // version of that module in an earlier `PYTHON_PATH`
                    // directory, we'll ignore the latest one.
                    //
                    // For example, if the module `foo_sdk` appears twice in
                    // `PYTHON_PATH`, and both versions have a
                    // componentize-py.toml file, we'll ignore the second one
                    // just as Python will ignore the second module.

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
                    config: toml::from_str::<RawComponentizePyConfig>(
                        &fs::read_to_string(&path).with_context(|| path.display().to_string())?,
                    )?,
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

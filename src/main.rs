#![deny(warnings)]

use {
    anyhow::{bail, Context, Result},
    clap::Parser,
    std::{
        env, fs,
        io::{self, Cursor, Seek},
        path::{Path, PathBuf},
        process::Command,
        str,
    },
    tar::Archive,
    wizer::Wizer,
    zstd::Decoder,
};

#[cfg(unix)]
const NATIVE_PATH_DELIMITER: char = ':';

#[cfg(windows)]
const NATIVE_PATH_DELIMITER: char = ';';

/// A utility to convert Python apps into Wasm components
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Options {
    /// The name of a Python module containing the app to wrap
    app_name: String,

    /// File or directory containing WIT document(s)
    #[arg(short = 'd', long, default_value = "wit")]
    wit_path: PathBuf,

    /// Name of world to target (or default world if `None`)
    #[arg(short = 'w', long)]
    world: Option<String>,

    /// `PYTHONPATH` for specifying directory containing the app and optionally other directories containing
    /// dependencies.
    ///
    /// If `pipenv` is in `$PATH` and `pipenv --venv` produces a path containing a `site-packages` subdirectory,
    /// that directory will be appended to this value as a convenience for `pipenv` users.
    #[arg(short = 'p', long, default_value = ".")]
    python_path: String,

    /// Output file to write the resulting module to
    #[arg(short = 'o', long, default_value = "index.wasm")]
    output: PathBuf,
}

#[derive(Parser, Debug)]
struct PrivateOptions {
    app_name: String,
    wit_path: PathBuf,
    #[arg(long)]
    world: Option<String>,
    python_home: String,
    python_path: String,
    output: PathBuf,
    wit_path: PathBuf,
}

fn main() -> Result<()> {
    if env::var_os("COMPONENTIZE_PY_WIZEN").is_some() {
        let options = PrivateOptions::parse();

        env::remove_var("COMPONENTIZE_PY_WIZEN");

        env::set_var("PYTHONUNBUFFERED", "1");
        env::set_var("COMPONENTIZE_PY_APP_NAME", &options.app_name);

        let mut wizer = Wizer::new();

        wizer
            .allow_wasi(true)?
            .inherit_env(true)
            .inherit_stdio(true)
            .wasm_bulk_memory(true);

        let python_path = options
            .python_path
            .split(NATIVE_PATH_DELIMITER)
            .enumerate()
            .map(|(index, path)| {
                let index = index.to_string();
                wizer.map_dir(&index, path);
                format!("/{index}")
            })
            .collect::<Vec<_>>()
            .join(":");

        wizer.map_dir("python", &options.python_home);

        env::set_var("PYTHONPATH", format!("/python:{python_path}"));
        env::set_var("PYTHONHOME", "/python");

        let module = wizer.run(&zstd::decode_all(Cursor::new(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/runtime.wasm.zst"
        ))))?)?;

        let component = componentize(
            &module,
            &parse_wit(&options.wit_path, options.wit_world.as_deref())?,
        )?;

        fs::write(&options.output, component)?;
    } else {
        let options = Options::parse();

        let temp = tempfile::tempdir()?;

        Archive::new(Decoder::new(Cursor::new(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/python-lib.tar.zst"
        ))))?)
        .unpack(temp.path())?;

        let mut python_path = options.python_path;
        if let Some(site_packages) = find_site_packages()? {
            python_path = format!(
                "{python_path}{NATIVE_PATH_DELIMITER}{}",
                site_packages
                    .to_str()
                    .context("non-UTF-8 site-packages name")?
            )
        }

        // Spawn a subcommand to do the real work.  This gives us an opportunity to clear the environment so that
        // build-time environment variables don't end up in the Wasm module we're building.
        //
        // Note that we need to use temporary files for stdio instead of the default inheriting behavior since (as
        // of this writing) CPython interacts poorly with Wasmtime's WASI implementation if any of the stdio
        // descriptors point to non-files on Windows.  Specifically, the WASI implementation will trap when CPython
        // calls `fd_filestat_get` on non-files.

        let mut stdin = tempfile::tempfile()?;
        let mut stdout = tempfile::tempfile()?;
        let mut stderr = tempfile::tempfile()?;

        let summary = summarize(&parse_wit(&options.wit_path, options.wit_world.as_deref())?)?;
        bincode::serialize_into(&mut stdin, &summary)?;
        stdin.rewind()?;

        let mut cmd = Command::new(env::args().next().unwrap());
        cmd.env_clear()
            .env("COMPONENTIZE_PY_WIZEN", "1")
            .arg(&options.app_name)
            .arg(&options.wit_path)
            .arg(
                temp.path()
                    .to_str()
                    .context("non-UTF-8 temporary directory name")?,
            )
            .arg(&python_path)
            .arg(&options.output)
            .stdin(stdin)
            .stdout(stdout.try_clone()?)
            .stderr(stderr.try_clone()?);

        if let Some(world) = &options.world {
            cmd.arg("--world").arg(world);
        }

        let status = cmd.status()?;

        if !status.success() {
            stdout.rewind()?;
            io::copy(&mut stdout, &mut io::stdout().lock())?;

            stderr.rewind()?;
            io::copy(&mut stderr, &mut io::stderr().lock())?;

            bail!("Couldn't create wasm from input");
        }

        println!("Component built successfully");
    }

    Ok(())
}

fn find_site_packages() -> Result<Option<PathBuf>> {
    Ok(match Command::new("pipenv").arg("--venv").output() {
        Ok(output) => {
            if output.status.success() {
                let dir = Path::new(str::from_utf8(&output.stdout)?.trim()).join("lib");

                if let Some(site_packages) = find_dir("site-packages", &dir)? {
                    Some(site_packages)
                } else {
                    eprintln!(
                        "warning: site-packages directory not found under {}",
                        dir.display()
                    );
                    None
                }
            } else {
                // `pipenv` is in `$PATH`, but this app does not appear to be using it
                None
            }
        }
        Err(_) => {
            // `pipenv` is not in `$PATH -- assume this app isn't using it
            None
        }
    })
}

fn find_dir(name: &str, path: &Path) -> Result<Option<PathBuf>> {
    if path.is_dir() {
        match path.file_name().and_then(|name| name.to_str()) {
            Some(this_name) if this_name == name => {
                return Ok(Some(path.canonicalize()?));
            }
            _ => {
                for entry in fs::read_dir(path)? {
                    if let Some(path) = find_dir(name, &entry?.path())? {
                        return Ok(Some(path));
                    }
                }
            }
        }
    }

    Ok(None)
}

fn parse_wit(path: &Path, world: Option<&str>) -> Result<(Resolve, WorldId)> {
    let mut resolve = Resolve::default();
    let pkg = if path.is_dir() {
        resolve.push_dir(&path)?.0
    } else {
        let pkg = UnresolvedPackage::parse_file(path)?;
        resolve.push(pkg, &Default::default())?
    };
    let world = resolve.select_world(pkg, world.as_deref())?;
    Ok((resolve, world))
}

fn visit_type_def(
    resolve: &Resolve,
    type_def: &wit_parser::TypeDef,
    types: &mut Vec<Type>,
    type_map: &mut HashMap<TypeId, usize>,
) -> TypeDef {
    TypeDef {
        name: type_def.name.to_owned(),
        kind: match &type_def.kind {
            wit_parser::TypeDef::Record(record) => TypeDef::Record {
                fields: record
                    .fields
                    .iter()
                    .map(|field| {
                        (
                            field.name.to_owned(),
                            visit_type(resolve, field.ty, types, type_map),
                        )
                    })
                    .collect(),
            },
            wit_parser::TypeDef::Flags(flags) => TypeDef::Flags {
                flags: flags
                    .flags
                    .iter()
                    .map(|flag| flag.name().to_owned())
                    .collect(),
            },
            wit_parser::TypeDef::Tuple(tuple) => TypeDef::Tuple {
                types: record
                    .types
                    .iter()
                    .map(|ty| visit_type(resolve, *ty, types, type_map))
                    .collect(),
            },
            wit_parser::TypeDef::Variant(variant) => TypeDef::Variant {
                cases: record
                    .cases
                    .iter()
                    .map(|case| {
                        (
                            case.name.to_owned(),
                            case.ty.map(|ty| visit_type(resolve, ty, types, type_map)),
                        )
                    })
                    .collect(),
            },
            wit_parser::TypeDef::Enum(en) => TypeDef::Enum {
                cases: record.cases.iter().map(|case| case.name.to_owned()),
            },
            wit_parser::TypeDef::Option(ty) => {
                TypeDef::Option(visit_type(resolve, *ty, types, type_map))
            }
            wit_parser::TypeDef::Result(result) => TypeDef::Result {
                ok: result.ok.map(|ty| visit_type(resolve, ty, types, type_map)),
                err: result
                    .err
                    .map(|ty| visit_type(resolve, ty, types, type_map)),
            },
            wit_parser::TypeDef::Union(union) => TypeDef::Union {
                cases: union
                    .cases
                    .iter()
                    .map(|case| visit_type(resolve, case.ty, types, type_map))
                    .collect(),
            },
            wit_parser::TypeDef::List(ty) => {
                TypeDef::List(visit_type(resolve, *ty, types, type_map))
            }
            wit_parser::TypeDef::Future(ty) => {
                TypeDef::Future(ty.map(|ty| visit_type(resolve, ty, types, type_map)))
            }
            wit_parser::TypeDef::Stream(stream) => TypeDef::Stream {
                element: result
                    .element
                    .map(|ty| visit_type(resolve, ty, types, type_map)),
                end: result
                    .end
                    .map(|ty| visit_type(resolve, ty, types, type_map)),
            },
            wit_parser::TypeDef::Type(ty) => {
                TypeDef::Type(visit_type(resolve, *ty, types, type_map))
            }
            Unknown => unreachable!(),
        },
    }
}

fn visit_type(
    resolve: &Resolve,
    ty: wit_parser::Type,
    types: &mut Vec<Type>,
    type_map: &mut HashMap<TypeId, usize>,
) -> Type {
    match ty {
        wit_parser::Type::Bool => Type::Bool,
        wit_parser::Type::U8 => Type::U8,
        wit_parser::Type::U16 => Type::U16,
        wit_parser::Type::U32 => Type::U32,
        wit_parser::Type::U64 => Type::U64,
        wit_parser::Type::S8 => Type::S8,
        wit_parser::Type::S16 => Type::S16,
        wit_parser::Type::S32 => Type::S32,
        wit_parser::Type::S64 => Type::S64,
        wit_parser::Type::Float32 => Type::Float32,
        wit_parser::Type::Float64 => Type::Float64,
        wit_parser::Type::Char => Type::Char,
        wit_parser::Type::String => Type::String,
        wit_parser::Type::Id(id) => Type::Id(if let Some(index) = type_map.get(id) {
            *index
        } else {
            let type_def = visit_type_def(resolve, &resolve.types[id], types, type_map);
            types.push(type_def);
            let n = types.len() - 1;
            type_map.insert(*id, n);
            n
        }),
    }
}

fn visit_params(
    resolve: &Resolve,
    params: &[(String, Type)],
    types: &mut Vec<Type>,
    type_map: &mut HashMap<TypeId, usize>,
) -> Vec<(String, Type)> {
    params
        .iter()
        .map(|(name, ty)| (name.to_owned(), visit_type(resolve, ty, types, type_map)))
        .collect()
}

fn visit_results(
    resolve: &Resolve,
    results: &wit_parser::Results,
    types: &mut Vec<Type>,
    type_map: &mut HashMap<TypeId, usize>,
) -> Results {
    match results {
        wit_parser::Results::Named(named) => {
            Results::Named(visit_params(resolve, named, types, type_map))
        }
        wit_parser::Results::Anon(ty) => visit_type(resolve, ty, types, type_map),
    }
}

fn visit_items(
    resolve: &Resolve,
    items: &IndexMap<String, WorldItem>,
    types: &mut Vec<Type>,
    type_map: &mut HashMap<TypeId, usize>,
) -> Result<Vec<Function>> {
    let mut funcs = Vec::new();
    for (item_name, item) in items {
        match item {
            WorldItem::Interface(interface) => {
                for (func_name, func) in &resolve.interfaces[interface].functions {
                    funcs.push(Function {
                        name: format!("{item_name}#{func_name}"),
                        params: visit_params(resolve, &func.params, types, type_map)?,
                        results: visit_results(resolve, &func.results, types, type_map)?,
                    })
                }
            }

            WorldItem::Function(func) => funcs.push(Function {
                name: func.name.to_owned(),
                params: visit_params(resolve, &func.params, types, type_map)?,
                results: visit_results(resolve, &func.results, types, type_map)?,
            }),

            WorldItem::Type(_) => bail!("type imports and exports not yet supported"),
        }
    }
    Ok(funcs)
}

fn summarize((resolve, world): &(Resolve, WorldId)) -> Result<Summary> {
    // Generate a `Vec<Type>` and two `Vec<Function>`s of imports and exports, respectively, which reference the
    // former.

    let mut types = Vec::new();
    let mut type_map = HashMap::new();
    let imports = visit_items(
        resolve,
        &resolve.worlds[world].imports,
        &mut types,
        &mut type_map,
    )?;
    let exports = visit_items(
        resolve,
        &resolve.worlds[world].exports,
        &mut types,
        &mut type_map,
    )?;

    // Also generate a Python script which declares the types and the imports (which pass their arguments in an
    // array to a low-level `call_import` function defined in Rust, which in turn marshals them using the canonical
    // ABI and calls the real `call_import`).
}

fn componentize(module: &[u8], (resolve, world): &(Resolve, WorldId)) -> Result<Vec<u8>> {
    // Locate, remember, and remove low-level import (`call_import`) and export (`call_export`).
    // Also, locate and remember stack pointer global.

    // Generate and insert component imports, exports, and function table

    // Generate and append component type custom section

    // Encode with WASI Preview 1 adapter
    Ok(ComponentEncoder::default()
        .validate(true)
        .module(&module)?
        .adapter(
            "wasi_snapshot_preview1",
            &zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/wasi_snapshot_preview1.wasm.zst"
            ))))?,
        )?
        .encode()?)
}

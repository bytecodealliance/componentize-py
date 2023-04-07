#![deny(warnings)]

use {
    anyhow::{bail, Context, Result},
    clap::Parser as _,
    std::{
        env,
        fs::{self, File},
        io::{self, Cursor, Seek},
        path::{Path, PathBuf},
        process::Command,
        str,
    },
    summary::Summary,
    tar::Archive,
    wit_parser::{Resolve, UnresolvedPackage, WorldId},
    wizer::Wizer,
    zstd::Decoder,
};

mod abi;
mod bindgen;
mod componentize;
mod convert;
mod summary;
mod util;

#[cfg(unix)]
const NATIVE_PATH_DELIMITER: char = ':';

#[cfg(windows)]
const NATIVE_PATH_DELIMITER: char = ';';

/// A utility to convert Python apps into Wasm components
#[derive(clap::Parser, Debug)]
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

    /// Output file to which to write the resulting component
    #[arg(short = 'o', long, default_value = "index.wasm")]
    output: PathBuf,

    /// Disable non-error output
    #[arg(short = 'q', long)]
    quiet: bool,
}

#[derive(clap::Parser, Debug)]
struct PrivateOptions {
    app_name: String,
    #[arg(long)]
    world: Option<String>,
    python_home: String,
    python_path: String,
    output: PathBuf,
    wit_path: PathBuf,
}

fn main() -> Result<()> {
    if env::var_os("COMPONENTIZE_PY_COMPONENTIZE").is_some() {
        componentize(PrivateOptions::parse())
    } else {
        fork(Options::parse())
    }
}

fn fork(options: Options) -> Result<()> {
    // Spawn a subcommand to do the real work.  This gives us an opportunity to clear the environment so that
    // build-time environment variables don't end up in the Wasm module we're building.
    //
    // Note that we need to use temporary files for stdio instead of the default inheriting behavior since (as
    // of this writing) CPython interacts poorly with Wasmtime's WASI implementation if any of the stdio
    // descriptors point to non-files on Windows.  Specifically, the WASI implementation will trap when CPython
    // calls `fd_filestat_get` on non-files.

    let stdlib = tempfile::tempdir()?;

    Archive::new(Decoder::new(Cursor::new(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/python-lib.tar.zst"
    ))))?)
    .unpack(stdlib.path())?;

    let mut python_path = options.python_path;

    if let Some(site_packages) = find_site_packages()? {
        python_path = format!(
            "{python_path}{NATIVE_PATH_DELIMITER}{}",
            site_packages
                .to_str()
                .context("non-UTF-8 site-packages name")?
        )
    }

    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;

    let mut cmd = Command::new(env::args().next().unwrap());
    cmd.env_clear()
        .env("COMPONENTIZE_PY_COMPONENTIZE", "1")
        .arg(&options.app_name)
        .arg(
            stdlib
                .path()
                .to_str()
                .context("non-UTF-8 temporary directory name")?,
        )
        .arg(&python_path)
        .arg(&options.output)
        .arg(&options.wit_path)
        .stdin(tempfile::tempfile()?)
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?);

    if let Some(world) = &options.world {
        cmd.arg("--world").arg(world);
    }

    let status = cmd.status()?;

    stdout.rewind()?;
    io::copy(&mut stdout, &mut io::stdout().lock())?;

    stderr.rewind()?;
    io::copy(&mut stderr, &mut io::stderr().lock())?;

    if !status.success() {
        bail!("Couldn't create wasm from input");
    }

    if !options.quiet {
        println!("Component built successfully");
    }

    Ok(())
}

fn componentize(options: PrivateOptions) -> Result<()> {
    env::remove_var("COMPONENTIZE_PY_COMPONENTIZE");

    env::set_var("PYTHONUNBUFFERED", "1");
    env::set_var("COMPONENTIZE_PY_APP_NAME", &options.app_name);

    let mut wizer = Wizer::new();

    wizer
        .allow_wasi(true)?
        .inherit_env(true)
        .inherit_stdio(true)
        .wasm_bulk_memory(true);

    let (resolve, world) = parse_wit(&options.wit_path, options.world.as_deref())?;
    let summary = Summary::try_new(&resolve, world)?;

    let symbols = tempfile::tempdir()?;
    wizer.map_dir("symbols", symbols.path());
    bincode::serialize_into(
        &mut File::create(symbols.path().join("bin"))?,
        &summary.collect_symbols(),
    )?;
    env::set_var("COMPONENTIZE_PY_SYMBOLS_PATH", "/symbols/bin");

    let generated_code = tempfile::tempdir()?;
    summary.generate_code(generated_code.path())?;

    let python_path = format!(
        "{}{NATIVE_PATH_DELIMITER}{}",
        options.python_path,
        generated_code
            .path()
            .to_str()
            .context("non-UTF-8 temporary directory name")?
    );

    let python_path = python_path
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

    let component = componentize::componentize(&module, &resolve, world, &summary)?;

    fs::write(&options.output, component)?;

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
        resolve.push_dir(path)?.0
    } else {
        let pkg = UnresolvedPackage::parse_file(path)?;
        resolve.push(pkg, &Default::default())?
    };
    let world = resolve.select_world(pkg, world)?;
    Ok((resolve, world))
}

use {
    anyhow::{Context, Result},
    clap::Parser as _,
    std::{
        fs,
        path::{Path, PathBuf},
        process, str,
    },
};

/// A utility to convert Python apps into Wasm components
#[derive(clap::Parser, Debug)]
#[command(author, version, about)]
struct Options {
    #[command(flatten)]
    common: Common,

    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args, Debug)]
struct Common {
    /// File or directory containing WIT document(s)
    #[arg(short = 'd', long, default_value = "wit")]
    wit_path: PathBuf,

    /// Name of world to target (or default world if `None`)
    #[arg(short = 'w', long)]
    world: Option<String>,

    /// Disable non-error output
    #[arg(short = 'q', long)]
    quiet: bool,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    /// Generate a component from the specified Python app and its dependencies.
    Componentize(Componentize),

    /// Generate Python bindings for the world and write them to the specified directory.
    Bindings(Bindings),
}

#[derive(clap::Args, Debug)]
struct Componentize {
    /// The name of a Python module containing the app to wrap
    app_name: String,

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

    /// If specified, replace all WASI imports with trapping stubs.
    ///
    /// If this is set, the generated component will not have access to any WASI functionality, e.g. filesystem,
    /// environment variables, network, etc. at runtime.  The only imports allowed are those specified by the
    /// world.
    #[arg(long)]
    stub_wasi: bool,
}

#[derive(clap::Args, Debug)]
struct Bindings {
    /// Directory to which bindings should be written.
    ///
    /// This will be created if it does not already exist.
    output_dir: PathBuf,
}

fn main() -> Result<()> {
    let options = Options::parse();
    match options.command {
        Command::Componentize(opts) => componentize(options.common, opts),
        Command::Bindings(opts) => generate_bindings(options.common, opts),
    }
}

fn generate_bindings(common: Common, bindings: Bindings) -> Result<()> {
    componentize_py::generate_bindings(
        &common.wit_path,
        common.world.as_deref(),
        &bindings.output_dir,
    )
}

fn componentize(common: Common, componentize: Componentize) -> Result<()> {
    let mut python_path = componentize.python_path;

    if let Some(site_packages) = find_site_packages()? {
        python_path = format!(
            "{python_path}{}{}",
            componentize_py::NATIVE_PATH_DELIMITER,
            site_packages
                .to_str()
                .context("non-UTF-8 site-packages name")?
        )
    }

    componentize_py::componentize(
        &common.wit_path,
        common.world.as_deref(),
        &python_path,
        &componentize.app_name,
        componentize.stub_wasi,
        &componentize.output,
    )?;

    if !common.quiet {
        println!("Component built successfully");
    }

    Ok(())
}

fn find_site_packages() -> Result<Option<PathBuf>> {
    Ok(
        match process::Command::new("pipenv").arg("--venv").output() {
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
        },
    )
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

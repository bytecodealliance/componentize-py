use {
    anyhow::{Context, Result},
    clap::Parser as _,
    std::{
        env,
        ffi::OsString,
        fs,
        path::{Path, PathBuf},
        process, str,
    },
    tokio::runtime::Runtime,
};

/// A utility to convert Python apps into Wasm components
#[derive(clap::Parser, Debug)]
#[command(author, version, about)]
pub struct Options {
    #[command(flatten)]
    pub common: Common,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(clap::Args, Debug)]
pub struct Common {
    /// File or directory containing WIT document(s)
    #[arg(short = 'd', long)]
    pub wit_path: Option<PathBuf>,

    /// Name of world to target (or default world if `None`)
    #[arg(short = 'w', long)]
    pub world: Option<String>,

    /// Disable non-error output
    #[arg(short = 'q', long)]
    pub quiet: bool,
}

#[derive(clap::Subcommand, Debug)]
pub enum Command {
    /// Generate a component from the specified Python app and its dependencies.
    Componentize(Componentize),

    /// Generate Python bindings for the world and write them to the specified directory.
    Bindings(Bindings),
}

#[derive(clap::Args, Debug)]
pub struct Componentize {
    /// The name of a Python module containing the app to wrap
    pub app_name: String,

    /// Specify a directory containing the app and/or its dependencies.  May be specified more than once.
    ///
    /// If a `VIRTUAL_ENV` environment variable is set, it will be interpreted as a directory name, and that
    /// directory will be searched for a `site-packages` subdirectory, which will be appended to the path as a
    /// convenience for `venv` users.  Alternatively, if `pipenv` is in `$PATH` and `pipenv --venv` produces a
    /// non-empty result, it will be searched for a `site-packages` subdirectory, which will likewise be appended.
    /// If the previous options fail, the `site` module in python will be used to get the `site-packages`
    #[arg(short = 'p', long, default_value = ".")]
    pub python_path: Vec<String>,

    /// Output file to which to write the resulting component
    #[arg(short = 'o', long, default_value = "index.wasm")]
    pub output: PathBuf,
}

#[derive(clap::Args, Debug)]
pub struct Bindings {
    /// Directory to which bindings should be written.
    ///
    /// This will be created if it does not already exist.
    pub output_dir: PathBuf,

    /// Optional name of top-level module to use for bindings.
    ///
    /// If this is not specified, the module name will be derived from the world name.
    #[arg(long)]
    pub world_module: Option<String>,
}

pub fn run<T: Into<OsString> + Clone, I: IntoIterator<Item = T>>(args: I) -> Result<()> {
    let options = Options::parse_from(args);
    match options.command {
        Command::Componentize(opts) => componentize(options.common, opts),
        Command::Bindings(opts) => generate_bindings(options.common, opts),
    }
}

fn generate_bindings(common: Common, bindings: Bindings) -> Result<()> {
    crate::generate_bindings(
        &common
            .wit_path
            .unwrap_or_else(|| Path::new("wit").to_owned()),
        common.world.as_deref(),
        bindings.world_module.as_deref(),
        &bindings.output_dir,
    )
}

fn componentize(common: Common, componentize: Componentize) -> Result<()> {
    let mut python_path = componentize.python_path;

    if let Some(site_packages) = find_site_packages()? {
        python_path.push(
            site_packages
                .to_str()
                .context("non-UTF-8 site-packages name")?
                .to_owned(),
        );
    }

    Runtime::new()?.block_on(crate::componentize(
        common.wit_path.as_deref(),
        common.world.as_deref(),
        &python_path.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        &componentize.app_name,
        &componentize.output,
        None,
    ))?;

    if !common.quiet {
        println!("Component built successfully");
    }

    Ok(())
}

fn find_site_packages() -> Result<Option<PathBuf>> {
    Ok(if let Ok(env) = env::var("VIRTUAL_ENV") {
        let dir = Path::new(&env).join("lib");

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
        let pipenv_packages = match process::Command::new("pipenv").arg("--venv").output() {
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
        };

        if pipenv_packages.is_some() {
            pipenv_packages
        } else {
            // Get site packages location using the `site` module in python
            match process::Command::new("python3")
                .args(["-c", "import site; print(site.getsitepackages()[0])"])
                .output()
            {
                Ok(output) => {
                    let path = Path::new(str::from_utf8(&output.stdout)?.trim()).to_path_buf();
                    Some(path)
                }
                Err(_) => None,
            }
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

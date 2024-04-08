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

    /// Optional `isyswasfa` suffix.
    ///
    /// If this is specified, the generated component will use [isyswasfa](https://github.com/dicej/isyswasfa) to
    /// polyfill composable concurrency.
    #[arg(long)]
    pub isyswasfa: Option<String>,
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

    /// Specify which world to use with which Python module.  May be specified more than once.
    ///
    /// Some Python modules (e.g. SDK wrappers around WIT APIs) may contain `componentize-py.toml` files which
    /// point to embedded WIT files, and those may define multiple WIT worlds.  In this case, it may be necessary
    /// to specify which world on a module-by-module basis.
    ///
    /// Note that these must be specified in topological order (i.e. if a module containing WIT files depends on
    /// other modules containing WIT files, it must be listed after all its dependencies).
    #[arg(short = 'm', long, value_parser = parse_module_world)]
    pub module_worlds: Vec<(String, String)>,

    /// Output file to which to write the resulting component
    #[arg(short = 'o', long, default_value = "index.wasm")]
    pub output: PathBuf,

    /// If set, replace all WASI imports with trapping stubs.
    ///
    /// PLEASE NOTE: This has the effect of baking whatever PRNG seed is generated at build time into the
    /// component, meaning Python's `random` module will return the exact same sequence each time the component is
    /// run.  Do *not* use this option in situations where a secure source of randomness is required.
    #[arg(short = 's', long)]
    pub stub_wasi: bool,
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

fn parse_module_world(s: &str) -> Result<(String, String), String> {
    let (k, v) = s
        .split_once('=')
        .ok_or_else(|| format!("expected string of form `<key>=<value>`; got `{s}`"))?;
    Ok((k.to_string(), v.to_string()))
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
        common.isyswasfa.as_deref(),
    )
}

fn componentize(common: Common, componentize: Componentize) -> Result<()> {
    let mut python_path = componentize.python_path;

    for site_packages in find_site_packages()? {
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
        &componentize
            .module_worlds
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect::<Vec<_>>(),
        &componentize.app_name,
        &componentize.output,
        None,
        common.isyswasfa.as_deref(),
        componentize.stub_wasi,
    ))?;

    if !common.quiet {
        println!("Component built successfully");
    }

    Ok(())
}

fn find_site_packages() -> Result<Vec<PathBuf>> {
    Ok(if let Ok(env) = env::var("VIRTUAL_ENV") {
        let dir = Path::new(&env).join("lib");

        if let Some(site_packages) = find_dir("site-packages", &dir)? {
            vec![site_packages]
        } else {
            eprintln!(
                "warning: site-packages directory not found under {}",
                dir.display()
            );
            Vec::new()
        }
    } else {
        let pipenv_packages = match process::Command::new("pipenv").arg("--venv").output() {
            Ok(output) => {
                if output.status.success() {
                    let dir = Path::new(str::from_utf8(&output.stdout)?.trim()).join("lib");

                    if let Some(site_packages) = find_dir("site-packages", &dir)? {
                        vec![site_packages]
                    } else {
                        eprintln!(
                            "warning: site-packages directory not found under {}",
                            dir.display()
                        );
                        Vec::new()
                    }
                } else {
                    // `pipenv` is in `$PATH`, but this app does not appear to be using it
                    Vec::new()
                }
            }
            Err(_) => {
                // `pipenv` is not in `$PATH -- assume this app isn't using it
                Vec::new()
            }
        };

        if !pipenv_packages.is_empty() {
            pipenv_packages
        } else {
            // Get site packages location using the `site` module in python
            match process::Command::new("python3")
                .args([
                    "-c",
                    "import site; \
                     list = site.getsitepackages(); \
                     list.insert(0, site.getusersitepackages()); \
                     print(';'.join(list))",
                ])
                .output()
            {
                Ok(output) => str::from_utf8(&output.stdout)?
                    .trim()
                    .split(';')
                    .map(|p| Path::new(p).to_path_buf())
                    .collect(),
                Err(_) => Vec::new(),
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

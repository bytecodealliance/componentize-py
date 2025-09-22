use {
    anyhow::{Context, Result},
    clap::Parser as _,
    serde::Serialize,
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

#[derive(clap::Args, Clone, Debug)]
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

    /// Comma-separated list of features that should be enabled when processing
    /// WIT files.
    ///
    /// This enables using `@unstable` annotations in WIT files.
    #[clap(long)]
    features: Vec<String>,

    /// Whether or not to activate all WIT features when processing WIT files.
    ///
    /// This enables using `@unstable` annotations in WIT files.
    #[clap(long)]
    all_features: bool,

    /// Specify names to use for imported interfaces.  May be specified more than once.
    ///
    /// By default, the python module name generated for a given interface will be the snake-case form of the WIT
    /// interface name, possibly qualified with the package name and namespace and/or version if that name would
    /// otherwise clash with another interface.  With this option, you may override that name with your own, unique
    /// name.
    #[arg(long, value_parser = parse_key_value)]
    pub import_interface_name: Vec<(String, String)>,

    /// Specify names to use for exported interfaces.  May be specified more than once.
    ///
    /// By default, the python module name generated for a given interface will be the snake-case form of the WIT
    /// interface name, possibly qualified with the package name and namespace and/or version if that name would
    /// otherwise clash with another interface.  With this option, you may override that name with your own, unique
    /// name.
    #[arg(long, value_parser = parse_key_value)]
    pub export_interface_name: Vec<(String, String)>,

    /// Optional name of top-level module to use for bindings.
    ///
    /// If this is not specified, the module name will default to "wit_world".
    #[arg(long)]
    pub world_module: Option<String>,
}

#[derive(clap::Subcommand, Debug)]
pub enum Command {
    /// Generate a component from the specified Python app and its dependencies.
    Componentize(Componentize),

    /// Generate Python bindings for the world and write them to the specified directory.
    Bindings(Bindings),
}

#[derive(clap::ValueEnum, Clone, Default, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WasiAdapter {
    /// The "reactor" adapter provides the default adaptation from preview1 to preview2.
    ///
    /// This adapter implements the wasi:cli/imports world.
    #[default]
    Reactor,
    /// The "command" adapter extends the “reactor” adapter and additionally exports a run function entrypoint.
    ///
    /// This adapter implements the wasi:cli/command world.
    Command,
    /// The “proxy” adapter provides implements a HTTP proxy which is more restricted than the "reactor" adapter
    /// adapter, as it lacks filesystem, socket, environment, exit, and terminal support, but includes HTTP
    /// handlers for incoming and outgoing requests.
    ///
    /// This adapter implements the wasi:http/proxy world.
    Proxy,
}

#[derive(clap::Args, Debug)]
pub struct Componentize {
    /// The name of a Python module containing the app to wrap.
    ///
    /// Note that this should not match (any of) the world name(s) you are targeting since `componentize-py` will
    /// generate code using those name(s), and Python doesn't know how to load two top-level modules with the same
    /// name.
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
    #[arg(short = 'm', long, value_parser = parse_key_value)]
    pub module_worlds: Vec<(String, String)>,

    /// Output file to which to write the resulting component
    #[arg(short = 'o', long, default_value = "index.wasm")]
    pub output: PathBuf,

    /// Adapter to use
    #[arg(short = 'a', long, default_value = "reactor")]
    pub adapter: WasiAdapter,

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
}

fn parse_key_value(s: &str) -> Result<(String, String), String> {
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
        &common.features,
        common.all_features,
        common.world_module.as_deref(),
        &bindings.output_dir,
        &common
            .import_interface_name
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect(),
        &common
            .export_interface_name
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect(),
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
        &common.features,
        common.all_features,
        common.world_module.as_deref(),
        &python_path.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        &componentize
            .module_worlds
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect::<Vec<_>>(),
        &componentize.app_name,
        &componentize.output,
        None,
        componentize.adapter,
        componentize.stub_wasi,
        &common
            .import_interface_name
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect(),
        &common
            .export_interface_name
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect(),
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

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    /// Generates a WIT file which has unstable feature "x"
    fn gated_x_wit_file() -> Result<tempfile::NamedTempFile, anyhow::Error> {
        let mut wit = tempfile::Builder::new()
            .prefix("gated")
            .suffix(".wit")
            .tempfile()?;
        write!(
            wit,
            r#"
            package foo:bar@1.2.3;

            world bindings {{
                @unstable(feature = x)
                import x: func();
                @since(version = 1.2.3)
                export y: func();
            }}
        "#,
        )?;
        Ok(wit)
    }

    #[test]
    fn unstable_bindings_not_generated() -> Result<()> {
        // Given a WIT file with gated features
        let wit = gated_x_wit_file()?;
        let out_dir = tempfile::tempdir()?;

        // When generating the bindings for this WIT world
        let common = Common {
            wit_path: Some(wit.path().into()),
            world: None,
            world_module: Some("bindings".into()),
            quiet: false,
            features: vec![],
            all_features: false,
            import_interface_name: Vec::new(),
            export_interface_name: Vec::new(),
        };
        let bindings = Bindings {
            output_dir: out_dir.path().into(),
        };
        generate_bindings(common, bindings)?;

        // Then the gated feature doesn't appear
        let generated = fs::read_to_string(out_dir.path().join("bindings/__init__.py"))?;

        assert!(!generated.contains("def x() -> None:"));

        Ok(())
    }

    #[test]
    fn unstable_bindings_generated_with_feature_flag() -> Result<()> {
        // Given a WIT file with gated features
        let wit = gated_x_wit_file()?;
        let out_dir = tempfile::tempdir()?;

        // When generating the bindings for this WIT world
        let common = Common {
            wit_path: Some(wit.path().into()),
            world: None,
            world_module: Some("bindings".into()),
            quiet: false,
            features: vec!["x".to_owned()],
            all_features: false,
            import_interface_name: Vec::new(),
            export_interface_name: Vec::new(),
        };
        let bindings = Bindings {
            output_dir: out_dir.path().into(),
        };
        generate_bindings(common, bindings)?;

        // Then the gated feature doesn't appear
        let generated = fs::read_to_string(out_dir.path().join("bindings/__init__.py"))?;

        assert!(generated.contains("def x() -> None:"));

        Ok(())
    }

    #[test]
    fn unstable_bindings_generated_for_all_features() -> Result<()> {
        // Given a WIT file with gated features
        let wit = gated_x_wit_file()?;
        let out_dir = tempfile::tempdir()?;

        // When generating the bindings for this WIT world
        let common = Common {
            wit_path: Some(wit.path().into()),
            world: None,
            world_module: Some("bindings".into()),
            quiet: false,
            features: vec![],
            all_features: true,
            import_interface_name: Vec::new(),
            export_interface_name: Vec::new(),
        };
        let bindings = Bindings {
            output_dir: out_dir.path().into(),
        };
        generate_bindings(common, bindings)?;

        // Then the gated feature doesn't appear
        let generated = fs::read_to_string(out_dir.path().join("bindings/__init__.py"))?;

        assert!(generated.contains("def x() -> None:"));

        Ok(())
    }

    #[test]
    fn unstable_features_used_in_componentize() -> Result<()> {
        // Given bindings to a WIT file with gated features and a Python file that uses them
        let wit = gated_x_wit_file()?;
        let out_dir = tempfile::tempdir()?;
        let common = Common {
            wit_path: Some(wit.path().into()),
            world: None,
            world_module: Some("bindings".into()),
            quiet: false,
            features: vec!["x".to_owned()],
            all_features: false,
            import_interface_name: Vec::new(),
            export_interface_name: Vec::new(),
        };
        let bindings = Bindings {
            output_dir: out_dir.path().into(),
        };
        generate_bindings(common.clone(), bindings)?;
        fs::write(
            out_dir.path().join("app.py"),
            r#"
import bindings
from bindings import x

class Bindings(bindings.Bindings):
    def y(self) -> None:
        x()
"#,
        )?;

        // Building the component succeeds
        let componentize_opts = Componentize {
            app_name: "app".to_owned(),
            python_path: vec![out_dir.path().to_string_lossy().into()],
            module_worlds: vec![],
            output: out_dir.path().join("app.wasm"),
            stub_wasi: false,
        };
        componentize(common, componentize_opts)
    }
}

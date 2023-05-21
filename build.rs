#![deny(warnings)]

use {
    anyhow::{bail, Result},
    std::{
        env,
        fmt::Write as _,
        fs::{self, File},
        io::{self, Write},
        path::{Path, PathBuf},
        process::Command,
    },
    tar::Builder,
    zstd::Encoder,
};

const ZSTD_COMPRESSION_LEVEL: i32 = 19;

#[cfg(any(target_os = "macos", target_os = "windows"))]
const PYTHON_EXECUTABLE: &str = "python.exe";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const PYTHON_EXECUTABLE: &str = "python";

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());

    if matches!(env::var("CARGO_CFG_FEATURE").as_deref(), Ok("cargo-clippy"))
        || env::var("CLIPPY_ARGS").is_ok()
    {
        stubs_for_clippy(&out_dir)
    } else {
        package_all_the_things(&out_dir)
    }?;

    // TODO: how can we detect `cargo test` and only run this in that case (or more specifically, run it so it
    // generates an empty file)?
    test_generator::generate()
}

fn stubs_for_clippy(out_dir: &Path) -> Result<()> {
    println!(
        "cargo:warning=using stubbed runtime, core library, and adapter for static analysis purposes..."
    );

    let runtime_path = out_dir.join("runtime.wasm.zst");

    if !runtime_path.exists() {
        Encoder::new(File::create(runtime_path)?, ZSTD_COMPRESSION_LEVEL)?.do_finish()?;
    }

    let core_library_path = out_dir.join("python-lib.tar.zst");

    if !core_library_path.exists() {
        Builder::new(Encoder::new(
            File::create(core_library_path)?,
            ZSTD_COMPRESSION_LEVEL,
        )?)
        .into_inner()?
        .do_finish()?;
    }

    let wasi_adapter_path = out_dir.join("wasi_snapshot_preview1.wasm.zst");

    if !wasi_adapter_path.exists() {
        Builder::new(Encoder::new(
            File::create(wasi_adapter_path)?,
            ZSTD_COMPRESSION_LEVEL,
        )?)
        .into_inner()?
        .do_finish()?;
    }

    Ok(())
}

fn package_all_the_things(out_dir: &Path) -> Result<()> {
    let repo_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());

    maybe_make_cpython(&repo_dir);

    make_pyo3_config(&repo_dir);

    let mut cmd = Command::new("cargo");
    cmd.arg("build")
        .current_dir("runtime")
        .arg("--release")
        .arg("--target=wasm32-wasi")
        .env("CARGO_TARGET_DIR", out_dir)
        .env("PYO3_CONFIG_FILE", out_dir.join("pyo3-config.txt"));

    let status = cmd.status()?;
    assert!(status.success());
    println!("cargo:rerun-if-changed=runtime");

    let runtime_path = out_dir.join("wasm32-wasi/release/componentize_py_runtime.wasm");

    if runtime_path.exists() {
        let copied_runtime_path = out_dir.join("runtime.wasm.zst");

        let mut encoder = Encoder::new(File::create(copied_runtime_path)?, ZSTD_COMPRESSION_LEVEL)?;
        io::copy(&mut File::open(runtime_path)?, &mut encoder)?;
        encoder.do_finish()?;
    } else {
        bail!("no such file: {}", runtime_path.display())
    }

    let core_library_path = repo_dir.join("cpython/builddir/wasi/install/lib/python3.11");

    if core_library_path.exists() {
        let copied_core_library_path = out_dir.join("python-lib.tar.zst");

        let mut builder = Builder::new(Encoder::new(
            File::create(copied_core_library_path)?,
            ZSTD_COMPRESSION_LEVEL,
        )?);

        add(&mut builder, &core_library_path, &core_library_path)?;

        builder.into_inner()?.do_finish()?;
    } else {
        bail!("no such directory: {}", core_library_path.display())
    }

    let mut cmd = Command::new("cargo");
    cmd.arg("build")
        .current_dir("preview2")
        .arg("--release")
        .arg("--target=wasm32-unknown-unknown")
        .env("CARGO_TARGET_DIR", out_dir);

    let status = cmd.status()?;
    assert!(status.success());
    println!("cargo:rerun-if-changed=preview2");

    let adapter_path = out_dir.join("wasm32-unknown-unknown/release/wasi_snapshot_preview1.wasm");
    let copied_adapter_path = out_dir.join("wasi_snapshot_preview1.wasm.zst");
    let mut encoder = Encoder::new(File::create(copied_adapter_path)?, ZSTD_COMPRESSION_LEVEL)?;
    io::copy(&mut File::open(adapter_path)?, &mut encoder)?;
    encoder.do_finish()?;

    Ok(())
}

fn include(path: &Path) -> bool {
    !(matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("a" | "pyc" | "whl")
    ) || matches!(
        path.file_name().and_then(|e| e.to_str()),
        Some("Makefile" | "Changelog" | "NEWS.txt")
    ))
}

fn add(builder: &mut Builder<impl Write>, root: &Path, path: &Path) -> Result<()> {
    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            add(builder, root, &entry?.path())?;
        }
    } else if include(path) {
        builder.append_file(path.strip_prefix(root)?, &mut File::open(path)?)?;
    }

    Ok(())
}

fn maybe_make_cpython(repo_dir: &Path) {
    let cpython_wasi_dir = repo_dir.join("cpython/builddir/wasi");
    if !cpython_wasi_dir.join("libpython3.11.a").exists() {
        let cpython_native_dir = repo_dir.join("cpython/builddir/build");
        if !cpython_native_dir.join(PYTHON_EXECUTABLE).exists() {
            fs::create_dir_all(&cpython_native_dir).unwrap();
            fs::create_dir_all(&cpython_wasi_dir).unwrap();

            run(Command::new("../../configure")
                .current_dir(&cpython_native_dir)
                .arg(format!(
                    "--prefix={}/install",
                    cpython_native_dir.to_str().unwrap()
                )));

            run(Command::new("make").current_dir(cpython_native_dir));
        }

        let config_guess = run(Command::new("../../config.guess").current_dir(&cpython_wasi_dir));

        run(Command::new("../../Tools/wasm/wasi-env")
            .env("CONFIG_SITE", "../../Tools/wasm/config.site-wasm32-wasi")
            .current_dir(&cpython_wasi_dir)
            .args([
                "../../configure",
                "-C",
                "--host=wasm32-unknown-wasi",
                &format!("--build={}", String::from_utf8(config_guess).unwrap()),
                &format!(
                    "--with-build-python={}/../build/{PYTHON_EXECUTABLE}",
                    cpython_wasi_dir.to_str().unwrap()
                ),
                &format!("--prefix={}/install", cpython_wasi_dir.to_str().unwrap()),
                "--disable-test-modules",
            ]));

        run(Command::new("make")
            .current_dir(cpython_wasi_dir)
            .arg("install"));
    }
}

fn run(command: &mut Command) -> Vec<u8> {
    let output = command.output().unwrap();
    if output.status.success() {
        output.stdout
    } else {
        panic!(
            "command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn make_pyo3_config(repo_dir: &Path) {
    let out_dir = env::var("OUT_DIR").unwrap();
    let mut cpython_wasi_dir = repo_dir.join("cpython/builddir/wasi");
    let mut cygpath = Command::new("cygpath");
    cygpath.arg("-w").arg(&cpython_wasi_dir);
    if let Ok(output) = cygpath.output() {
        if output.status.success() {
            cpython_wasi_dir = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
        } else {
            panic!(
                "cygpath failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    let mut pyo3_config = fs::read_to_string(repo_dir.join("pyo3-config.txt")).unwrap();
    writeln!(
        pyo3_config,
        "lib_dir={}",
        cpython_wasi_dir.to_str().unwrap()
    )
    .unwrap();
    fs::write(Path::new(&out_dir).join("pyo3-config.txt"), pyo3_config).unwrap();

    println!("cargo:rerun-if-changed=pyo3-config.txt");
}

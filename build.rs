#![deny(warnings)]

use {
    anyhow::{anyhow, bail, Result},
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
        || env::var("CARGO_EXPAND_NO_RUN_NIGHTLY").is_ok()
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

    let files = [
        "libcomponentize_py_runtime.so.zst",
        "libpython3.11.so.zst",
        "libc.so.zst",
        "libwasi-emulated.so.zst",
        "libc++.so.zst",
        "libc++abi.so.zst",
        "wasi_snapshot_preview1.wasm.zst",
    ];

    for file in files {
        let path = out_dir.join(file);

        if !path.exists() {
            Encoder::new(File::create(path)?, ZSTD_COMPRESSION_LEVEL)?.do_finish()?;
        }
    }

    let path = out_dir.join("python-lib.tar.zst");

    if !path.exists() {
        Builder::new(Encoder::new(File::create(path)?, ZSTD_COMPRESSION_LEVEL)?)
            .into_inner()?
            .do_finish()?;
    }

    Ok(())
}

fn package_all_the_things(out_dir: &Path) -> Result<()> {
    let repo_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());

    let wasi_sdk =
        PathBuf::from(env::var_os("WASI_SDK_PATH").unwrap_or_else(|| "/opt/wasi-sdk".into()));

    eprintln!("using wasi_sdk: {}", wasi_sdk.display());

    maybe_make_cpython(&repo_dir, &wasi_sdk);

    let cpython_wasi_dir = repo_dir.join("cpython/builddir/wasi");

    make_pyo3_config(&repo_dir);

    let mut cmd = Command::new("rustup");
    cmd.current_dir("runtime")
        .arg("run")
        .arg("nightly")
        .arg("cargo")
        .arg("build")
        .arg("-Z")
        .arg("build-std=panic_abort,std")
        .arg("--release")
        .arg("--target=wasm32-wasi");

    for (key, value) in env::vars_os() {
        if key
            .to_str()
            .map(|key| key.starts_with("RUST") || key.starts_with("CARGO"))
            .unwrap_or(false)
        {
            eprintln!(
                "removing {}: {}",
                key.to_string_lossy(),
                value.to_string_lossy()
            );
            cmd.env_remove(&key);
        } else {
            eprintln!(
                "keeping {}: {}",
                key.to_string_lossy(),
                value.to_string_lossy()
            );
        }
    }

    cmd.env("RUSTFLAGS", "-C relocation-model=pic")
        .env("CARGO_TARGET_DIR", out_dir)
        .env("PYO3_CONFIG_FILE", out_dir.join("pyo3-config.txt"));

    let status = cmd.status()?;
    assert!(status.success());
    println!("cargo:rerun-if-changed=runtime");

    let path = out_dir.join("wasm32-wasi/release/libcomponentize_py_runtime.a");

    if path.exists() {
        let clang = wasi_sdk.join("bin/clang").canonicalize()?;
        if clang.exists() {
            let name = "libcomponentize_py_runtime.so";

            run(Command::new(clang)
                .arg("-shared")
                .arg("-o")
                .arg(&out_dir.join(name).canonicalize()?)
                .arg("-Wl,--whole-archive")
                .arg(&path.canonicalize()?)
                .arg("-Wl,--no-whole-archive")
                .arg(format!(
                    "-L{}",
                    cpython_wasi_dir.canonicalize()?.to_str().unwrap()
                ))
                .arg("-lpython3.11"));

            compress(out_dir, name, out_dir, false)?;
        } else {
            bail!("no such file: {}", clang.display())
        }
    } else {
        bail!("no such file: {}", path.display())
    }

    let libraries = [
        "libc.so",
        "libwasi-emulated.so",
        "libc++.so",
        "libc++abi.so",
    ];

    for library in libraries {
        compress(
            &wasi_sdk.join("share/wasi-sysroot/lib/wasm32-wasi"),
            library,
            out_dir,
            true,
        )?;
    }

    compress(&cpython_wasi_dir, "libpython3.11.so", out_dir, true)?;

    let path = repo_dir.join("cpython/builddir/wasi/install/lib/python3.11");

    if path.exists() {
        let mut builder = Builder::new(Encoder::new(
            File::create(out_dir.join("python-lib.tar.zst"))?,
            ZSTD_COMPRESSION_LEVEL,
        )?);

        add(&mut builder, &path, &path)?;

        builder.into_inner()?.do_finish()?;
    } else {
        bail!("no such directory: {}", path.display())
    }

    let mut cmd = Command::new("cargo");
    cmd.arg("build")
        .current_dir("wasmtime/crates/wasi-preview1-component-adapter")
        .arg("--release")
        .arg("--target=wasm32-unknown-unknown")
        .env("CARGO_TARGET_DIR", out_dir);

    let status = cmd.status()?;
    assert!(status.success());
    println!("cargo:rerun-if-changed=wasmtime");

    compress(
        &out_dir.join("wasm32-unknown-unknown/release"),
        "wasi_snapshot_preview1.wasm",
        out_dir,
        false,
    )?;

    Ok(())
}

fn compress(src_dir: &Path, name: &str, dst_dir: &Path, rerun_if_changed: bool) -> Result<()> {
    let path = src_dir.join(name);

    if rerun_if_changed {
        println!("cargo:rerun-if-changed={}", path.to_str().unwrap());
    }

    if path.exists() {
        let mut encoder = Encoder::new(
            File::create(dst_dir.join(format!("{name}.zst")))?,
            ZSTD_COMPRESSION_LEVEL,
        )?;
        io::copy(&mut File::open(path)?, &mut encoder)?;
        encoder.do_finish()?;
        Ok(())
    } else {
        Err(anyhow!("no such file: {}", path.display()))
    }
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

fn maybe_make_cpython(repo_dir: &Path, wasi_sdk: &Path) {
    let cpython_wasi_dir = repo_dir.join("cpython/builddir/wasi");
    if !cpython_wasi_dir.join("libpython3.11.so").exists() {
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

            let config_guess =
                run(Command::new("../../config.guess").current_dir(&cpython_wasi_dir));

            run(Command::new("../../Tools/wasm/wasi-env")
                .env("CONFIG_SITE", "../../Tools/wasm/config.site-wasm32-wasi")
                .env("CFLAGS", "-fPIC")
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
                .current_dir(&cpython_wasi_dir)
                .arg("install"));
        }

        run(Command::new(wasi_sdk.join("bin/clang"))
            .arg("-shared")
            .arg("-o")
            .arg(cpython_wasi_dir.join("libpython3.11.so"))
            .arg("-Wl,--whole-archive")
            .arg(cpython_wasi_dir.join("libpython3.11.a"))
            .arg("-Wl,--no-whole-archive")
            .arg(cpython_wasi_dir.join("Modules/_decimal/libmpdec/libmpdec.a"))
            .arg(cpython_wasi_dir.join("Modules/expat/libexpat.a")));
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

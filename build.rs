#![deny(warnings)]

use {
    anyhow::{bail, Result},
    std::{
        env,
        fs::{self, File},
        io::{self, Write},
        path::{Path, PathBuf},
        process::Command,
    },
    tar::Builder,
    zstd::Encoder,
};

const ZSTD_COMPRESSION_LEVEL: i32 = 19;

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());

    if matches!(env::var("CARGO_CFG_FEATURE").as_deref(), Ok("cargo-clippy"))
        || env::var("CLIPPY_ARGS").is_ok()
    {
        stubs_for_clippy(&out_dir)
    } else {
        package_runtime_and_core_library(&out_dir)
    }
}

fn stubs_for_clippy(out_dir: &Path) -> Result<()> {
    println!(
        "cargo:warning=using stubbed runtime and core library for static analysis purposes..."
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

fn package_runtime_and_core_library(out_dir: &Path) -> Result<()> {
    let repo_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());

    let mut cmd = Command::new("cargo");
    cmd.arg("build")
        .current_dir("runtime")
        .arg("--release")
        .arg("--target=wasm32-wasi")
        .env("CARGO_TARGET_DIR", out_dir);

    let status = cmd.status()?;
    assert!(status.success());
    println!("cargo:rerun-if-changed=runtime");

    let runtime_path = out_dir.join("target/wasm32-wasi/release/spin_python_runtime.wasm");

    println!("cargo:rerun-if-changed={runtime_path:?}");

    if runtime_path.exists() {
        let copied_runtime_path = out_dir.join("runtime.wasm.zst");

        let mut encoder = Encoder::new(File::create(copied_runtime_path)?, ZSTD_COMPRESSION_LEVEL)?;
        io::copy(&mut File::open(runtime_path)?, &mut encoder)?;
        encoder.do_finish()?;
    } else {
        bail!("no such file: {}", runtime_path.display())
    }

    let core_library_path = repo_dir.join("cpython/builddir/wasi/install/lib/python3.11");

    println!("cargo:rerun-if-changed={core_library_path:?}");

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

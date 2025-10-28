#![deny(warnings)]

use {
    anyhow::{Context, Result, anyhow, bail},
    std::{
        env,
        fmt::Write as _,
        fs::{self, File},
        io::{self, Write},
        iter,
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

#[cfg(target_os = "windows")]
const CLANG_EXECUTABLE: &str = "clang.exe";
#[cfg(not(target_os = "windows"))]
const CLANG_EXECUTABLE: &str = "clang";

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
        "libcomponentize_py_runtime_sync.so.zst",
        "libcomponentize_py_runtime_async.so.zst",
        "libpython3.14.so.zst",
        "libc.so.zst",
        "libwasi-emulated-mman.so.zst",
        "libwasi-emulated-process-clocks.so.zst",
        "libwasi-emulated-getpid.so.zst",
        "libwasi-emulated-signal.so.zst",
        "libc++.so.zst",
        "libc++abi.so.zst",
        "wasi_snapshot_preview1.reactor.wasm.zst",
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

    let path = out_dir.join("bundled.tar.zst");

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

    maybe_make_cpython(&repo_dir, &wasi_sdk)?;

    let cpython_wasi_dir = repo_dir.join("cpython/builddir/wasi");

    make_pyo3_config(&repo_dir)?;

    make_runtime(
        out_dir,
        &wasi_sdk,
        &cpython_wasi_dir,
        false,
        "libcomponentize_py_runtime_sync.so",
    )?;
    make_runtime(
        out_dir,
        &wasi_sdk,
        &cpython_wasi_dir,
        true,
        "libcomponentize_py_runtime_async.so",
    )?;

    let libraries = [
        "libc.so",
        "libwasi-emulated-mman.so",
        "libwasi-emulated-process-clocks.so",
        "libwasi-emulated-getpid.so",
        "libwasi-emulated-signal.so",
        "libc++.so",
        "libc++abi.so",
    ];

    for library in libraries {
        compress(
            &wasi_sdk.join("share/wasi-sysroot/lib/wasm32-wasip2"),
            library,
            out_dir,
            true,
        )?;
    }

    compress(&cpython_wasi_dir, "libpython3.14.so", out_dir, true)?;

    let path = repo_dir.join("cpython/builddir/wasi/install/lib/python3.14");

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

    let path = repo_dir.join("bundled");

    if path.exists() {
        let mut builder = Builder::new(Encoder::new(
            File::create(out_dir.join("bundled.tar.zst"))?,
            ZSTD_COMPRESSION_LEVEL,
        )?);

        add(&mut builder, &path, &path)?;

        builder.into_inner()?.do_finish()?;
    } else {
        bail!("no such directory: {}", path.display())
    }

    compress(
        &repo_dir.join("adapters/ab5a4484"),
        "wasi_snapshot_preview1.reactor.wasm",
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
    println!("cargo:rerun-if-changed={}", path.to_str().unwrap());

    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            add(builder, root, &entry?.path())?;
        }
    } else if include(path) {
        builder.append_file(path.strip_prefix(root)?, &mut File::open(path)?)?;
    }

    Ok(())
}

fn maybe_make_cpython(repo_dir: &Path, wasi_sdk: &Path) -> Result<()> {
    let cpython_wasi_dir = repo_dir.join("cpython/builddir/wasi");
    if !cpython_wasi_dir.join("libpython3.14.so").exists() {
        fs::create_dir_all(&cpython_wasi_dir)?;
        if !cpython_wasi_dir.join("libpython3.14.a").exists() {
            let cpython_native_dir = repo_dir.join("cpython/builddir/build");
            if !cpython_native_dir.join(PYTHON_EXECUTABLE).exists() {
                fs::create_dir_all(&cpython_native_dir)?;

                run(Command::new("../../configure")
                    .current_dir(&cpython_native_dir)
                    .arg(format!(
                        "--prefix={}/install",
                        cpython_native_dir.to_str().unwrap()
                    )))?;

                run(Command::new("make").current_dir(cpython_native_dir))?;
            }

            let lib_install_dir = cpython_wasi_dir.join("deps");
            build_zlib(wasi_sdk, &lib_install_dir)?;

            let config_guess =
                run(Command::new("../../config.guess").current_dir(&cpython_wasi_dir))?;

            run(Command::new("../../Tools/wasm/wasi-env")
                .env(
                    "CONFIG_SITE",
                    "../../Tools/wasm/wasi/config.site-wasm32-wasi",
                )
                .env(
                    "CFLAGS",
                    format!(
                        "--target=wasm32-wasip2 -fPIC -I{}/deps/include",
                        cpython_wasi_dir.display()
                    ),
                )
                .env(
                    "LDFLAGS",
                    format!(
                        "--target=wasm32-wasip2 -L{}/deps/lib",
                        cpython_wasi_dir.display()
                    ),
                )
                .current_dir(&cpython_wasi_dir)
                .args([
                    "../../configure",
                    "-C",
                    "--host=wasm32-unknown-wasip2",
                    &format!("--build={}", String::from_utf8(config_guess)?),
                    &format!(
                        "--with-build-python={}/../build/{PYTHON_EXECUTABLE}",
                        cpython_wasi_dir.to_str().unwrap()
                    ),
                    &format!("--prefix={}/install", cpython_wasi_dir.to_str().unwrap()),
                    "--disable-test-modules",
                    "--enable-ipv6",
                ]))?;

            run(Command::new("make")
                .current_dir(&cpython_wasi_dir)
                .args(["build_all", "install"]))?;
        }

        run(Command::new(wasi_sdk.join("bin/clang"))
            .arg("--target=wasm32-wasip2")
            .arg("-shared")
            .arg("-o")
            .arg(cpython_wasi_dir.join("libpython3.14.so"))
            .arg("-Wl,--whole-archive")
            .arg(cpython_wasi_dir.join("libpython3.14.a"))
            .arg("-Wl,--no-whole-archive")
            .arg(cpython_wasi_dir.join("Modules/_hacl/libHacl_HMAC.a"))
            .arg(cpython_wasi_dir.join("Modules/_hacl/libHacl_Hash_BLAKE2.a"))
            .arg(cpython_wasi_dir.join("Modules/_hacl/libHacl_Hash_MD5.a"))
            .arg(cpython_wasi_dir.join("Modules/_hacl/libHacl_Hash_SHA1.a"))
            .arg(cpython_wasi_dir.join("Modules/_hacl/libHacl_Hash_SHA2.a"))
            .arg(cpython_wasi_dir.join("Modules/_hacl/libHacl_Hash_SHA3.a"))
            .arg(cpython_wasi_dir.join("Modules/_decimal/libmpdec/libmpdec.a"))
            .arg(cpython_wasi_dir.join("Modules/expat/libexpat.a"))
            .arg(cpython_wasi_dir.join("deps/lib/libz.a"))
            .arg("-lwasi-emulated-signal")
            .arg("-lwasi-emulated-getpid")
            .arg("-lwasi-emulated-process-clocks")
            .arg("-ldl"))?;
    }

    Ok(())
}

fn run(command: &mut Command) -> Result<Vec<u8>> {
    let command_string = iter::once(command.get_program())
        .chain(command.get_args())
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");

    let output = command.output().with_context({
        let command_string = command_string.clone();
        move || command_string
    })?;

    if output.status.success() {
        Ok(output.stdout)
    } else {
        bail!(
            "command `{command_string}` failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn make_pyo3_config(repo_dir: &Path) -> Result<()> {
    let out_dir = env::var("OUT_DIR")?;
    let mut cpython_wasi_dir = repo_dir.join("cpython/builddir/wasi");
    let mut cygpath = Command::new("cygpath");
    cygpath.arg("-w").arg(&cpython_wasi_dir);
    if let Ok(output) = cygpath.output() {
        if output.status.success() {
            cpython_wasi_dir = PathBuf::from(String::from_utf8(output.stdout)?.trim());
        } else {
            panic!(
                "cygpath failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    let mut pyo3_config = fs::read_to_string(repo_dir.join("pyo3-config.txt"))?;
    writeln!(
        pyo3_config,
        "lib_dir={}",
        cpython_wasi_dir.to_str().unwrap()
    )?;
    fs::write(Path::new(&out_dir).join("pyo3-config.txt"), pyo3_config)?;

    println!("cargo:rerun-if-changed=pyo3-config.txt");

    Ok(())
}

fn make_runtime(
    out_dir: &Path,
    wasi_sdk: &Path,
    cpython_wasi_dir: &Path,
    async_: bool,
    name: &str,
) -> Result<()> {
    let mut cmd = Command::new("rustup");
    cmd.current_dir("runtime")
        .arg("run")
        .arg("nightly")
        .arg("cargo")
        .arg("build")
        .arg("-Z")
        .arg("build-std=panic_abort,std")
        .arg("--release")
        .arg("--target=wasm32-wasip1");

    if async_ {
        cmd.arg("--features=async");
    }

    for (key, _) in env::vars_os() {
        if key
            .to_str()
            .map(|key| key.starts_with("RUST") || key.starts_with("CARGO"))
            .unwrap_or(false)
        {
            cmd.env_remove(&key);
        }
    }

    let target = if async_ { "async" } else { "sync" };

    cmd.env(
        "RUSTFLAGS",
        "-C relocation-model=pic --cfg pyo3_disable_reference_pool",
    )
    .env("CARGO_TARGET_DIR", out_dir.join(target))
    .env("PYO3_CONFIG_FILE", out_dir.join("pyo3-config.txt"));

    let status = cmd.status()?;
    assert!(status.success());
    println!("cargo:rerun-if-changed=runtime");

    let path = out_dir
        .join(target)
        .join("wasm32-wasip1/release/libcomponentize_py_runtime.a");

    if path.exists() {
        let clang = wasi_sdk.join(format!("bin/{CLANG_EXECUTABLE}"));
        if clang.exists() {
            run(Command::new(clang)
                .arg("-shared")
                .arg("-o")
                .arg(out_dir.join(name))
                .arg("-Wl,--whole-archive")
                .arg(&path)
                .arg("-Wl,--no-whole-archive")
                .arg(format!("-L{}", cpython_wasi_dir.to_str().unwrap()))
                .arg("-lpython3.14"))?;

            compress(out_dir, name, out_dir, false)?;
        } else {
            bail!("no such file: {}", clang.display())
        }
    } else {
        bail!("no such file: {}", path.display())
    }

    Ok(())
}

fn fetch_extract(url: &str, out_dir: &Path) -> Result<()> {
    let response = reqwest::blocking::get(url)?;
    let decoder = flate2::read::GzDecoder::new(response);
    let mut archive = tar::Archive::new(decoder);
    archive.unpack(out_dir)?;
    Ok(())
}

fn add_compile_envs(wasi_sdk: &Path, command: &mut Command) {
    let sysroot = wasi_sdk.join("share/wasi-sysroot");
    let sysroot = sysroot.to_string_lossy();
    command
        .env("AR", wasi_sdk.join("bin/ar"))
        .env("CC", wasi_sdk.join("bin/clang"))
        .env("RANLIB", wasi_sdk.join("bin/ranlib"))
        .env(
            "CFLAGS",
            format!("--target=wasm32-wasi --sysroot={sysroot} -I{sysroot}/include/wasm32-wasip1 -D_WASI_EMULATED_SIGNAL -fPIC"),
        )
        .env(
            "LDFLAGS",
            format!("--target=wasm32-wasip2 --sysroot={sysroot} -L{sysroot}/lib -lwasi-emulated-signal")
        );
}

fn build_zlib(wasi_sdk: &Path, install_dir: &Path) -> Result<()> {
    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    fetch_extract(
        "https://github.com/madler/zlib/releases/download/v1.3.1/zlib-1.3.1.tar.gz",
        &out_dir,
    )?;
    let src_dir = out_dir.join("zlib-1.3.1");
    let mut configure = Command::new("./configure");
    add_compile_envs(wasi_sdk, &mut configure);
    configure
        .current_dir(&src_dir)
        .arg("--static")
        .arg(format!("--prefix={}", install_dir.display()));
    run(&mut configure)?;
    let mut make = Command::new("make");
    add_compile_envs(wasi_sdk, &mut make);
    make.current_dir(src_dir)
        .arg(format!("AR={}", wasi_sdk.join("bin/ar").display()))
        .arg("ARFLAGS=rcs")
        .arg(format!("CC={}", wasi_sdk.join("bin/clang").display()))
        .arg("static")
        .arg("install");
    run(&mut make)?;
    Ok(())
}

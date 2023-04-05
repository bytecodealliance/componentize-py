use std::{
    env,
    fmt::Write,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    let mut repo_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    repo_dir.pop();

    let wasi_sdk_path =
        std::env::var("WASI_SDK_PATH").unwrap_or_else(|_| "/opt/wasi-sdk".to_owned());

    maybe_make_cpython(&repo_dir);

    make_pyo3_config(&repo_dir);

    let repo_dir = repo_dir.to_str().unwrap();

    println!("cargo:rustc-link-search={wasi_sdk_path}/share/wasi-sysroot/lib/wasm32-wasi");
    println!("cargo:rustc-link-lib=wasi-emulated-signal");
    println!("cargo:rustc-link-lib=wasi-emulated-getpid");
    println!("cargo:rustc-link-lib=wasi-emulated-process-clocks");
    println!(
        "cargo:rustc-link-search={}/cpython/builddir/wasi/Modules/_decimal/libmpdec",
        repo_dir
    );
    println!("cargo:rustc-link-lib=mpdec");
    println!(
        "cargo:rustc-link-search={}/cpython/builddir/wasi/Modules/expat",
        repo_dir
    );
    println!("cargo:rustc-link-lib=expat");
    println!(
        "cargo:rustc-env=PYO3_CONFIG_FILE={}/pyo3-config.txt",
        env::var("OUT_DIR").unwrap()
    );
    println!("cargo:rerun-if-changed=pyo3-config.txt");
}

fn maybe_make_cpython(repo_dir: &Path) {
    let cpython_wasi_dir = repo_dir.join("cpython/builddir/wasi");
    if cpython_wasi_dir.join("libpython3.11.a").exists() {
        return;
    }

    let cpython_native_dir = repo_dir.join("cpython/builddir/build");
    fs::create_dir_all(&cpython_native_dir).unwrap();
    fs::create_dir_all(&cpython_wasi_dir).unwrap();

    run(Command::new("../../configure")
        .current_dir(&cpython_native_dir)
        .arg(format!(
            "--prefix={}/install",
            cpython_native_dir.to_str().unwrap()
        )));

    run(Command::new("make").current_dir(cpython_native_dir));

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
                "--with-build-python={}/../build/python",
                cpython_wasi_dir.to_str().unwrap()
            ),
            &format!("--prefix={}/install", cpython_wasi_dir.to_str().unwrap()),
            "--disable-test-modules",
        ]));

    run(Command::new("make")
        .current_dir(cpython_wasi_dir)
        .arg("install"));
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

    let mut pyo3_config = fs::read_to_string("pyo3-config.txt").unwrap();
    writeln!(
        pyo3_config,
        "lib_dir={}",
        cpython_wasi_dir.to_str().unwrap()
    )
    .unwrap();
    fs::write(Path::new(&out_dir).join("pyo3-config.txt"), pyo3_config).unwrap();
}

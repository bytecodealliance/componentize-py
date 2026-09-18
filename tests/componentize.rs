use core::net::Ipv4Addr;
use std::{
    fs,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    process::Stdio,
    thread::sleep,
    time::Duration,
};

use assert_cmd::{Command, cargo};
use flate2::bufread::GzDecoder;
use fs_extra::dir::CopyOptions;
use predicates::prelude::predicate;
use tar::Archive;

fn write_test_registry(directory: &Path, package: &str, wit: &str) -> anyhow::Result<PathBuf> {
    let (namespace, name) = package.split_once(':').unwrap();
    let mut resolve = wit_parser::Resolve::new();
    let package_id = resolve.push_str("registry.wit", wit)?;
    let version = resolve.packages[package_id]
        .name
        .version
        .as_ref()
        .unwrap()
        .to_string();
    let root = directory.join("registry");
    let package_path = root
        .join(namespace)
        .join(name)
        .join(format!("{version}.wasm"));
    fs::create_dir_all(package_path.parent().unwrap())?;
    fs::write(package_path, wit_component::encode(&resolve, package_id)?)?;

    let config = directory.join("registry.toml");
    fs::write(
        &config,
        format!(
            r#"[namespace_registries]
test = "test.invalid"

[registry."test.invalid".local]
root = {:?}
"#,
            root
        ),
    )?;
    Ok(config)
}

#[test]
fn componentizes_remote_target_without_local_wit() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let config = write_test_registry(
        dir.path(),
        "test:registry",
        "package test:registry@1.2.3;\nworld command { export marker: func() -> string; }",
    )?;
    fs::write(
        dir.path().join("app.py"),
        "import wit\n\n@wit.guest\nclass App(wit.WorldExports):\n    def marker(self) -> str:\n        return \"registry\"\n",
    )?;

    cargo::cargo_bin_cmd!("componentize-py")
        .current_dir(dir.path())
        .args([
            "--no-default-registries",
            "--registry-config",
            config.to_str().unwrap(),
            "-w",
            "test:registry/command@1.2.3",
            "componentize",
            "app",
            "-o",
            "app.wasm",
        ])
        .assert()
        .success();
    assert!(dir.path().join("app.wasm").is_file());
    Ok(())
}

#[test]
fn remote_intersector_preserves_local_default_target() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let config = write_test_registry(
        dir.path(),
        "test:registry",
        "package test:registry@1.2.3;\nworld limiter { export marker: func() -> u32; }",
    )?;
    fs::write(
        dir.path().join("local.wit"),
        "package test:local@1.0.0;\nworld local { export marker: func() -> string; }",
    )?;
    fs::write(
        dir.path().join("app.py"),
        "import wit\n\n@wit.guest\nclass App(wit.WorldExports):\n    def marker(self) -> str:\n        return \"local\"\n",
    )?;

    cargo::cargo_bin_cmd!("componentize-py")
        .current_dir(dir.path())
        .args([
            "--no-default-registries",
            "--registry-config",
            config.to_str().unwrap(),
            "-d",
            "local.wit",
            "componentize",
            "app",
            "--intersect-world",
            "test:registry/limiter@1.2.3",
            "-o",
            "app.wasm",
        ])
        .assert()
        .success();

    let bytes = fs::read(dir.path().join("app.wasm"))?;
    let wit_component::DecodedWasm::Component(resolve, world) = wit_component::decode(&bytes)?
    else {
        anyhow::bail!("expected a component");
    };
    let wit_parser::WorldItem::Function(marker) =
        &resolve.worlds[world].exports[&wit_parser::WorldKey::Name("marker".into())]
    else {
        anyhow::bail!("expected the marker function");
    };
    assert_eq!(marker.result, Some(wit_parser::Type::String));
    Ok(())
}

#[test]
fn cli_example() -> anyhow::Result<()> {
    test_cli_example("cli", "wasi:cli/command@0.2.0")
}

#[test]
fn cli_p3_example() -> anyhow::Result<()> {
    test_cli_example("cli-p3", "wasi:cli/command@0.3.0")
}

fn test_cli_example(name: &str, world: &str) -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    fs_extra::copy_items(
        &[format!("./examples/{name}").as_str(), "./wit"],
        dir.path(),
        &CopyOptions::new(),
    )?;
    let path = dir.path().join(name);

    cargo::cargo_bin_cmd!("componentize-py")
        .current_dir(&path)
        .args([
            "-d",
            "../wit",
            "-w",
            world,
            "componentize",
            "app",
            "-o",
            "cli.wasm",
        ])
        .assert()
        .success()
        .stdout("Component built successfully\n");

    Command::new("wasmtime")
        .current_dir(&path)
        .args(["run", "-Sp3", "-Wcomponent-model-async", "cli.wasm"])
        .assert()
        .success()
        .stdout("Hello, world!\n");

    Ok(())
}

#[test]
fn http_example() -> anyhow::Result<()> {
    test_http_example("http", "wasi:http/proxy@0.2.0", 8080)
}

#[test]
fn http_p3_example() -> anyhow::Result<()> {
    test_http_example("http-p3", "wasi:http/service@0.3.0", 8081)
}

fn test_http_example(name: &str, world: &str, port: u16) -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    fs_extra::copy_items(
        &[format!("./examples/{name}").as_str(), "./wit"],
        dir.path(),
        &CopyOptions::new(),
    )?;
    let path = dir.path().join(name);

    cargo::cargo_bin_cmd!("componentize-py")
        .current_dir(&path)
        .args([
            "-d",
            "../wit",
            "-w",
            world,
            "componentize",
            "app",
            "-o",
            "http.wasm",
        ])
        .assert()
        .success()
        .stdout("Component built successfully\n");

    let mut handle = std::process::Command::new("wasmtime")
        .current_dir(&path)
        .args([
            "serve",
            &format!("--addr=0.0.0.0:{port}"),
            "-Sp3,common",
            "-Wcomponent-model-async",
            "http.wasm",
        ])
        .spawn()?;

    let content = "’Twas brillig, and the slithy toves
        Did gyre and gimble in the wabe:
All mimsy were the borogoves,
        And the mome raths outgrabe.
";

    let client = reqwest::blocking::Client::new();

    let text = retry(|| {
        Ok(client
            .post(format!("http://127.0.0.1:{port}/echo"))
            .header("content-type", "text/plain")
            .body(content)
            .send()?
            .error_for_status()?
            .text()?)
    })?;
    assert!(text.ends_with(&content));

    let text = retry(|| {
        Ok(client
            .get(format!("http://127.0.0.1:{port}/hash-all"))
            .header("url", "https://webassembly.github.io/spec/core/")
            .header("url", "https://www.w3.org/groups/wg/wasm/")
            .header("url", "https://bytecodealliance.org/")
            .send()?
            .error_for_status()?
            .text()?)
    })?;
    assert!(text.contains("https://webassembly.github.io/spec/core/:"));
    assert!(text.contains("https://bytecodealliance.org/:"));
    assert!(text.contains("https://www.w3.org/groups/wg/wasm/:"));

    handle.kill()?;

    Ok(())
}

#[test]
fn matrix_math_example() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    fs_extra::copy_items(
        &["./examples/matrix-math", "./wit"],
        dir.path(),
        &CopyOptions::new(),
    )?;
    let path = dir.path().join("matrix-math");

    install_numpy(&path)?;

    cargo::cargo_bin_cmd!("componentize-py")
        .current_dir(&path)
        .args([
            "-d",
            "../wit",
            "-w",
            "matrix-math",
            "componentize",
            "app",
            "-o",
            "matrix-math.wasm",
        ])
        .assert()
        .success()
        .stdout("Component built successfully\n");

    Command::new("wasmtime")
        .current_dir(&path)
        .args([
            "run",
            "matrix-math.wasm",
            "[[1, 2], [4, 5], [6, 7]]",
            "[[1, 2, 3], [4, 5, 6]]",
        ])
        .assert()
        .success()
        .stdout("matrix_multiply received arguments [[1, 2], [4, 5], [6, 7]] and [[1, 2, 3], [4, 5, 6]]\n[[9, 12, 15], [24, 33, 42], [34, 47, 60]]\n");

    Ok(())
}

#[test]
fn sandbox_example() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    fs_extra::copy_items(&["./examples/sandbox"], dir.path(), &CopyOptions::new())?;
    let path = dir.path().join("sandbox");

    cargo::cargo_bin_cmd!("componentize-py")
        .current_dir(&path)
        .args([
            "-d",
            "sandbox.wit",
            "componentize",
            "--stub-wasi",
            "guest",
            "-o",
            "sandbox.wasm",
        ])
        .assert()
        .success()
        .stdout("Component built successfully\n");

    Command::new("python3")
        .current_dir(&path)
        .args(["-m", "venv", ".venv"])
        .assert()
        .success();

    Command::new(venv_path(&path).join("pip"))
        .current_dir(&path)
        .args(["install", "wasmtime==48.0.0"])
        .assert()
        .success();

    Command::new(venv_path(&path).join("python"))
        .current_dir(&path)
        .args(["host.py", "2 + 2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("result: 4"));

    Ok(())
}

#[test]
fn tcp_example() -> anyhow::Result<()> {
    test_tcp_example("tcp", "wasi:cli/command@0.2.0")
}

#[test]
fn tcp_p3_example() -> anyhow::Result<()> {
    test_tcp_example("tcp-p3", "wasi:cli/command@0.3.0")
}

fn test_tcp_example(name: &str, world: &str) -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    fs_extra::copy_items(
        &[format!("./examples/{name}").as_str(), "./wit"],
        dir.path(),
        &CopyOptions::new(),
    )?;
    let path = dir.path().join(name);

    cargo::cargo_bin_cmd!("componentize-py")
        .current_dir(&path)
        .args([
            "-d",
            "../wit",
            "-w",
            world,
            "componentize",
            "app",
            "-o",
            "tcp.wasm",
        ])
        .assert()
        .success()
        .stdout("Component built successfully\n");

    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();

    let tcp_handle = std::process::Command::new("wasmtime")
        .current_dir(&path)
        .args([
            "run",
            "-Sp3,inherit-network",
            "-Wcomponent-model-async",
            "tcp.wasm",
            &format!("127.0.0.1:{port}"),
        ])
        .stdout(Stdio::piped())
        .spawn()?;

    let (mut stream, _) = listener.accept()?;

    let mut buffer = vec![0; 256];
    let count = stream.read(&mut buffer)?;
    assert_eq!(String::from_utf8_lossy(&buffer[..count]), "hello, world!");

    stream.write_all(b"hello")?;

    let output = tcp_handle.wait_with_output()?;

    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "received: b'hello'\n"
    );

    assert!(output.status.success());

    Ok(())
}

fn retry<T>(mut func: impl FnMut() -> anyhow::Result<T>) -> anyhow::Result<T> {
    let times = 10;
    for i in 0..times {
        match func() {
            Ok(t) => {
                return Ok(t);
            }
            Err(err) => {
                if i == times - 1 {
                    return Err(err);
                } else {
                    sleep(Duration::from_millis(2_u64.pow(i) * 100));
                    continue;
                }
            }
        }
    }
    unreachable!()
}

fn venv_path(path: &Path) -> PathBuf {
    path.join(".venv")
        .join(if cfg!(windows) { "Scripts" } else { "bin" })
}

fn install_numpy(path: &Path) -> anyhow::Result<()> {
    let bytes = reqwest::blocking::get(
        "https://github.com/dicej/wasi-wheels/releases/download/v0.0.2/numpy-wasi.tar.gz",
    )?
    .error_for_status()?
    .bytes()?;

    Archive::new(GzDecoder::new(&bytes[..])).unpack(path)?;

    Ok(())
}

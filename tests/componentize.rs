use std::{
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    thread::sleep,
    time::Duration,
};

use assert_cmd::Command;
use flate2::bufread::GzDecoder;
use fs_extra::dir::CopyOptions;
use predicates::prelude::predicate;
use tar::Archive;

#[test]
fn cli_example() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    fs_extra::copy_items(
        &["./examples/cli", "./wit"],
        dir.path(),
        &CopyOptions::new(),
    )?;
    let path = dir.path().join("cli");

    Command::cargo_bin("componentize-py")?
        .current_dir(&path)
        .args([
            "-d",
            "../wit",
            "-w",
            "wasi:cli/command@0.2.0",
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
        .args(["run", "cli.wasm"])
        .assert()
        .success()
        .stdout("Hello, world!\n");

    Ok(())
}

#[test]
fn http_example() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    fs_extra::copy_items(
        &["./examples/http", "./wit"],
        dir.path(),
        &CopyOptions::new(),
    )?;
    let path = dir.path().join("http");

    Command::cargo_bin("componentize-py")?
        .current_dir(&path)
        .args([
            "-d",
            "../wit",
            "-w",
            "wasi:http/proxy@0.2.0",
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
        .args(["serve", "--wasi", "common", "http.wasm"])
        .spawn()?;

    let content = "’Twas brillig, and the slithy toves
        Did gyre and gimble in the wabe:
All mimsy were the borogoves,
        And the mome raths outgrabe.
";

    let client = reqwest::blocking::Client::new();

    let echo = || -> anyhow::Result<String> {
        Ok(client
            .post("http://127.0.0.1:8080/echo")
            .header("content-type", "text/plain")
            .body(content)
            .send()?
            .error_for_status()?
            .text()?)
    };

    let text = retry(echo)?;
    assert!(text.ends_with(&content));

    let hash_all = || -> anyhow::Result<String> {
        Ok(client
            .get("http://127.0.0.1:8080/hash-all")
            .header("url", "https://webassembly.github.io/spec/core/")
            .header("url", "https://www.w3.org/groups/wg/wasm/")
            .header("url", "https://bytecodealliance.org/")
            .send()?
            .error_for_status()?
            .text()?)
    };

    let text = retry(hash_all)?;
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

    Command::cargo_bin("componentize-py")?
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

    Command::cargo_bin("componentize-py")?
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
        .args(["install", "wasmtime"])
        .assert()
        .success();

    Command::new(venv_path(&path).join("python"))
        .current_dir(&path)
        .args([
            "-m",
            "wasmtime.bindgen",
            "sandbox.wasm",
            "--out-dir",
            "sandbox",
        ])
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
    let dir = tempfile::tempdir()?;
    fs_extra::copy_items(
        &["./examples/tcp", "./wit"],
        dir.path(),
        &CopyOptions::new(),
    )?;
    let path = dir.path().join("tcp");

    Command::cargo_bin("componentize-py")?
        .current_dir(&path)
        .args([
            "-d",
            "../wit",
            "-w",
            "wasi:cli/command@0.2.0",
            "componentize",
            "app",
            "-o",
            "tcp.wasm",
        ])
        .assert()
        .success()
        .stdout("Component built successfully\n");

    let listener = std::net::TcpListener::bind("127.0.0.1:3456")?;

    let tcp_handle = std::process::Command::new("wasmtime")
        .current_dir(&path)
        .args([
            "run",
            "--wasi",
            "inherit-network",
            "tcp.wasm",
            "127.0.0.1:3456",
        ])
        .stdout(Stdio::piped())
        .spawn()?;

    let (mut stream, _) = listener.accept()?;
    stream.write_all(b"hello")?;

    let output = tcp_handle.wait_with_output()?;

    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "received: b'hello'\n"
    );

    Ok(())
}

fn retry<T>(func: impl Fn() -> anyhow::Result<T>) -> anyhow::Result<T> {
    for i in 0..10 {
        match func() {
            Ok(t) => {
                return Ok(t);
            }
            Err(err) => {
                if i == 4 {
                    return Err(err);
                } else {
                    sleep(Duration::from_secs(1));
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
        "https://github.com/dicej/wasi-wheels/releases/download/v0.0.1/numpy-wasi.tar.gz",
    )?
    .error_for_status()?
    .bytes()?;

    Archive::new(GzDecoder::new(&bytes[..])).unpack(path)?;

    Ok(())
}

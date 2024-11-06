use assert_cmd::Command;
use fs_extra::dir::CopyOptions;
use predicates::prelude::predicate;

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

    Command::new("curl")
        .current_dir(&path)
        .args([
            "-i",
            "-H",
            "content-type: text/plain",
            "--retry-connrefused",
            "--retry",
            "5",
            "--data-binary",
            "@-",
            "http://127.0.0.1:8080/echo",
        ])
        .write_stdin(content)
        .assert()
        .success()
        .stdout(predicate::str::ends_with(content));

    Command::new("curl")
        .current_dir(&path)
        .args([
            "-i",
            "-H",
            "url: https://webassembly.github.io/spec/core/",
            "-H",
            "url: https://www.w3.org/groups/wg/wasm/",
            "-H",
            "url: https://bytecodealliance.org/",
            "--retry-connrefused",
            "--retry",
            "5",
            "http://127.0.0.1:8080/hash-all",
        ])
        .assert()
        .success()
        .stdout(predicate::str::ends_with("
https://webassembly.github.io/spec/core/: d08394b6dfa544179f7e438c54b690ee1ac120572267175d986f128025b92792
https://bytecodealliance.org/: c79c3ab89afdc9d8027626ca87457ccba7700aa963df8d3a5ce8c5daa92a830b
https://www.w3.org/groups/wg/wasm/: b9cd0d52238845d0e90120828ec66ac2de67ed008975696f31bd1b49af21200d
"));

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

    Command::new("curl")
        .current_dir(&path)
        .args([
            "-OL",
            "https://github.com/dicej/wasi-wheels/releases/download/v0.0.1/numpy-wasi.tar.gz",
        ])
        .assert()
        .success();

    Command::new("tar")
        .current_dir(&path)
        .args(["xf", "numpy-wasi.tar.gz"])
        .assert()
        .success();

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
        .current_dir(dir.path())
        .args(["-m", "venv", ".venv"])
        .assert()
        .success();

    Command::new("./.venv/bin/pip")
        .current_dir(dir.path())
        .args(["install", "wasmtime"])
        .assert()
        .success();

    Command::new("../.venv/bin/python")
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

    Command::new("../.venv/bin/python")
        .current_dir(&path)
        .args(["host.py", "2 + 2"])
        .assert()
        .success()
        .stdout("result: 4\n");

    Ok(())
}

use assert_cmd::Command;
use fs_extra::dir::CopyOptions;

#[test]
fn cli_example() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    fs_extra::copy_items(
        &["./examples/cli", "./wit"],
        dir.path(),
        &CopyOptions::new(),
    )?;

    Command::cargo_bin("componentize-py")?
        .current_dir(dir.path())
        .args([
            "-d",
            "wit",
            "-w",
            "wasi:cli/command@0.2.0",
            "componentize",
            "cli.app",
            "-o",
            "cli.wasm",
        ])
        .assert()
        .success()
        .stdout("Component built successfully\n");

    Command::new("wasmtime")
        .current_dir(dir.path())
        .args(["run", "cli.wasm"])
        .assert()
        .success()
        .stdout("Hello, world!\n");

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

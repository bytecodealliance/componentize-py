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

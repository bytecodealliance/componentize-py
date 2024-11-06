use assert_cmd::Command;

#[test]
fn cli_example() -> anyhow::Result<()> {
    let dir = "./examples/cli";

    Command::cargo_bin("componentize-py")?
        .current_dir(dir)
        .args([
            "-d",
            "../../wit",
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
        .current_dir(dir)
        .args(["run", "cli.wasm"])
        .assert()
        .success()
        .stdout("Hello, world!\n");

    Ok(())
}

use std::path::Path;

use assert_cmd::{assert::Assert, Command};
use fs_extra::dir::CopyOptions;
use predicates::{prelude::predicate, Predicate};

#[test]
fn lint_cli_bindings() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    fs_extra::copy_items(
        &["./examples/cli", "./wit"],
        dir.path(),
        &CopyOptions::new(),
    )?;
    let path = dir.path().join("cli");

    generate_bindings(&path, "wasi:cli/command@0.2.0")?;

    assert!(predicate::path::is_dir().eval(&path.join("command")));

    mypy_command(dir.path())
        .current_dir(&path)
        .args(["--strict", "."])
        .assert()
        .success()
        .stdout("Success: no issues found in 33 source files\n");

    Ok(())
}

fn generate_bindings(path: &Path, world: &str) -> Result<Assert, anyhow::Error> {
    Ok(Command::cargo_bin("componentize-py")?
        .current_dir(path)
        .args(["-d", "../wit", "-w", world, "bindings", "."])
        .assert()
        .success())
}

fn mypy_command(temp_dir: &Path) -> Command {
    Command::new("python3")
        .current_dir(temp_dir)
        .args(["-m", "venv", ".venv"])
        .assert()
        .success();

    Command::new("./.venv/bin/pip")
        .current_dir(temp_dir)
        .args(["install", "mypy"])
        .assert()
        .success();

    Command::new("../.venv/bin/mypy")
}

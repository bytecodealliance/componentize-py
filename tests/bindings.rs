use std::{ffi::OsStr, path::Path};

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

    mypy_check(&path, ["--strict", "."]);

    Ok(())
}

#[test]
fn lint_http_bindings() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    fs_extra::copy_items(
        &["./examples/http", "./wit"],
        dir.path(),
        &CopyOptions::new(),
    )?;
    let path = dir.path().join("http");

    generate_bindings(&path, "wasi:http/proxy@0.2.0")?;

    assert!(predicate::path::is_dir().eval(&path.join("proxy")));

    mypy_check(
        &path,
        [
            "--strict",
            // poll_loop.py has many errors that might not be worth adjusting at the moment, so ignore for now
            "--ignore-missing-imports",
            "-m",
            "app",
            "-p",
            "proxy",
        ],
    );

    Ok(())
}

#[test]
fn lint_matrix_math_bindings() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    fs_extra::copy_items(
        &["./examples/matrix-math", "./wit"],
        dir.path(),
        &CopyOptions::new(),
    )?;
    let path = dir.path().join("matrix-math");

    install_numpy(&path);

    generate_bindings(&path, "matrix-math")?;

    assert!(predicate::path::is_dir().eval(&path.join("matrix_math")));

    mypy_check(
        &path,
        [
            "--strict",
            // numpy doesn't pass
            "--follow-imports",
            "silent",
            "-m",
            "app",
            "-p",
            "matrix_math",
        ],
    );

    Ok(())
}

#[test]
fn lint_sandbox_bindings() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    fs_extra::copy_items(&["./examples/sandbox"], dir.path(), &CopyOptions::new())?;
    let path = dir.path().join("sandbox");

    Command::cargo_bin("componentize-py")?
        .current_dir(&path)
        .args(["-d", "sandbox.wit", "bindings", "."])
        .assert()
        .success();

    assert!(predicate::path::is_dir().eval(&path.join("sandbox")));

    mypy_check(&path, ["--strict", "-m", "guest", "-p", "sandbox"]);

    Ok(())
}

#[test]
fn lint_tcp_bindings() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    fs_extra::copy_items(
        &["./examples/tcp", "./wit"],
        dir.path(),
        &CopyOptions::new(),
    )?;
    let path = dir.path().join("tcp");

    generate_bindings(&path, "wasi:cli/command@0.2.0")?;

    assert!(predicate::path::is_dir().eval(&path.join("command")));

    mypy_check(&path, ["--strict", "."]);

    Ok(())
}

fn generate_bindings(path: &Path, world: &str) -> Result<Assert, anyhow::Error> {
    Ok(Command::cargo_bin("componentize-py")?
        .current_dir(path)
        .args(["-d", "../wit", "-w", world, "bindings", "."])
        .assert()
        .success())
}

fn mypy_check<I, S>(path: &Path, args: I) -> Assert
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new("python3")
        .current_dir(path)
        .args(["-m", "venv", ".venv"])
        .assert()
        .success();

    Command::new("./.venv/bin/pip")
        .current_dir(path)
        .args(["install", "mypy"])
        .assert()
        .success();

    Command::new("./.venv/bin/mypy")
        .current_dir(path)
        .args(args)
        .assert()
        .success()
        .stdout(
            predicate::str::is_match("^Success: no issues found in \\d+ source files\n$").unwrap(),
        )
}

fn install_numpy(path: &Path) {
    Command::new("curl")
        .current_dir(path)
        .args([
            "-OL",
            "https://github.com/dicej/wasi-wheels/releases/download/v0.0.1/numpy-wasi.tar.gz",
        ])
        .assert()
        .success();

    Command::new("tar")
        .current_dir(path)
        .args(["xf", "numpy-wasi.tar.gz"])
        .assert()
        .success();
}

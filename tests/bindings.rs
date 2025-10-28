use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use assert_cmd::{Command, assert::Assert, cargo};
use flate2::bufread::GzDecoder;
use fs_extra::dir::CopyOptions;
use predicates::{Predicate, prelude::predicate};
use tar::Archive;

#[test]
fn lint_cli_bindings() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    fs_extra::copy_items(
        &["./examples/cli", "./wit", "./bundled"],
        dir.path(),
        &CopyOptions::new(),
    )?;
    let path = dir.path().join("cli");

    generate_bindings(&path, "wasi:cli/command@0.2.0")?;

    assert!(predicate::path::is_dir().eval(&path.join("wit_world")));

    mypy_check(&path, ["--strict", "."]);

    Ok(())
}

#[test]
fn lint_http_bindings() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    fs_extra::copy_items(
        &["./examples/http", "./wit", "./bundled"],
        dir.path(),
        &CopyOptions::new(),
    )?;
    // poll_loop.py has many errors that might not be worth adjusting at the moment, so ignore for now
    fs::remove_file(dir.path().join("bundled/poll_loop.py")).unwrap();
    let path = dir.path().join("http");

    generate_bindings(&path, "wasi:http/proxy@0.2.0")?;

    assert!(predicate::path::is_dir().eval(&path.join("wit_world")));

    mypy_check(
        &path,
        [
            "--strict",
            // poll_loop.py has many errors that might not be worth adjusting at the moment, so ignore for now
            "--ignore-missing-imports",
            "-m",
            "app",
            "-p",
            "wit_world",
        ],
    );

    Ok(())
}

#[test]
fn lint_http_p3_bindings() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    fs_extra::copy_items(
        &["./examples/http-p3", "./wit", "./bundled"],
        dir.path(),
        &CopyOptions::new(),
    )?;
    let path = dir.path().join("http-p3");

    generate_bindings(&path, "wasi:http/proxy@0.3.0-rc-2025-09-16")?;

    assert!(predicate::path::is_dir().eval(&path.join("wit_world")));

    _ = dir.keep();

    mypy_check(&path, ["--strict", "-m", "app", "-p", "wit_world"]);

    Ok(())
}

#[test]
fn lint_matrix_math_bindings() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    fs_extra::copy_items(
        &["./examples/matrix-math", "./wit", "./bundled"],
        dir.path(),
        &CopyOptions::new(),
    )?;
    let path = dir.path().join("matrix-math");

    install_numpy(&path)?;

    generate_bindings(&path, "matrix-math")?;

    assert!(predicate::path::is_dir().eval(&path.join("wit_world")));

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
            "wit_world",
        ],
    );

    Ok(())
}

#[test]
fn lint_sandbox_bindings() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    fs_extra::copy_items(
        &["./examples/sandbox", "./bundled"],
        dir.path(),
        &CopyOptions::new(),
    )?;
    let path = dir.path().join("sandbox");

    cargo::cargo_bin_cmd!("componentize-py")
        .current_dir(&path)
        .args(["-d", "sandbox.wit", "bindings", "."])
        .assert()
        .success();

    assert!(predicate::path::is_dir().eval(&path.join("wit_world")));

    mypy_check(&path, ["--strict", "-m", "guest", "-p", "wit_world"]);

    Ok(())
}

#[test]
fn lint_tcp_bindings() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    fs_extra::copy_items(
        &["./examples/tcp", "./wit", "./bundled"],
        dir.path(),
        &CopyOptions::new(),
    )?;
    let path = dir.path().join("tcp");

    generate_bindings(&path, "wasi:cli/command@0.2.0")?;

    assert!(predicate::path::is_dir().eval(&path.join("wit_world")));

    mypy_check(&path, ["--strict", "."]);

    Ok(())
}

fn generate_bindings(path: &Path, world: &str) -> Result<Assert, anyhow::Error> {
    Ok(cargo::cargo_bin_cmd!("componentize-py")
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

    Command::new(venv_path(path).join("pip"))
        .current_dir(path)
        .args(["install", "mypy"])
        .assert()
        .success();

    Command::new(venv_path(path).join("mypy"))
        .current_dir(path)
        .env(
            "MYPYPATH",
            ["bundled"]
                .into_iter()
                .map(|v| path.parent().unwrap().join(v).to_str().unwrap().to_string())
                .collect::<Vec<_>>()
                .join(":"),
        )
        .args(args)
        .assert()
        .success()
        .stdout(predicate::str::is_match("Success: no issues found in \\d+ source files").unwrap())
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

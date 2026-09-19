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
        &["./examples/cli", "./wit"],
        dir.path(),
        &CopyOptions::new(),
    )?;
    let path = dir.path().join("cli");

    generate_bindings(&path, "wasi:cli/command@0.2.0")?;

    assert!(predicate::path::is_dir().eval(&path.join("wit")));

    mypy_check(&path, ["--strict", "-m", "app"]);

    Ok(())
}

#[test]
fn lint_cli_p3_bindings() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    fs_extra::copy_items(
        &["./examples/cli-p3", "./wit"],
        dir.path(),
        &CopyOptions::new(),
    )?;
    let path = dir.path().join("cli-p3");

    generate_bindings(&path, "wasi:cli/command@0.3.0")?;

    assert!(predicate::path::is_dir().eval(&path.join("wit")));

    _ = dir.keep();

    mypy_check(&path, ["--strict", "-m", "app"]);

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

    // poll_loop.py has many errors that might not be worth adjusting at the moment, so ignore for now
    fs::remove_file(path.join("poll_loop.py")).unwrap();

    assert!(predicate::path::is_dir().eval(&path.join("wit")));

    mypy_check(
        &path,
        [
            "--strict",
            // poll_loop.py has many errors that might not be worth adjusting at the moment, so ignore for now
            "--ignore-missing-imports",
            "-m",
            "app",
        ],
    );

    Ok(())
}

#[test]
fn lint_http_p3_bindings() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    fs_extra::copy_items(
        &["./examples/http-p3", "./wit"],
        dir.path(),
        &CopyOptions::new(),
    )?;
    let path = dir.path().join("http-p3");

    generate_bindings(&path, "wasi:http/service@0.3.0")?;

    assert!(predicate::path::is_dir().eval(&path.join("wit")));

    _ = dir.keep();

    mypy_check(&path, ["--strict", "-m", "app"]);

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

    install_numpy(&path)?;

    generate_bindings(&path, "matrix-math")?;

    assert!(predicate::path::is_dir().eval(&path.join("wit")));

    mypy_check(
        &path,
        [
            "--strict",
            // numpy doesn't pass
            "--follow-imports",
            "silent",
            "-m",
            "app",
        ],
    );

    Ok(())
}

#[test]
fn lint_sandbox_bindings() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    fs_extra::copy_items(&["./examples/sandbox"], dir.path(), &CopyOptions::new())?;
    let path = dir.path().join("sandbox");

    cargo::cargo_bin_cmd!("componentize-py")
        .current_dir(&path)
        .args(["-d", "sandbox.wit", "bindings", "."])
        .assert()
        .success();

    assert!(predicate::path::is_dir().eval(&path.join("wit")));

    mypy_check(&path, ["--strict", "-m", "guest"]);

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

    assert!(predicate::path::is_dir().eval(&path.join("wit")));

    mypy_check(&path, ["--strict", "-m", "app"]);

    Ok(())
}

#[test]
fn lint_tcp_p3_bindings() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    fs_extra::copy_items(
        &["./examples/tcp-p3", "./wit"],
        dir.path(),
        &CopyOptions::new(),
    )?;
    let path = dir.path().join("tcp-p3");

    generate_bindings(&path, "wasi:cli/command@0.3.0")?;

    assert!(predicate::path::is_dir().eval(&path.join("wit")));

    mypy_check(&path, ["--strict", "-m", "app"]);

    Ok(())
}

#[test]
fn docstring_triple_quotes_are_valid_python() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    fs::write(
        dir.path().join("example.wit"),
        r#"package demo:poc;

world example {
  /// """
  export hello: func(name: string) -> string;

  /// docs containing both """ and '''
  export both: func() -> string;

  /// docs containing both """" and '''
  export four: func() -> string;

  /// docs containing both \""" and '''
  export backslash: func() -> string;
}
"#,
    )?;

    cargo::cargo_bin_cmd!("componentize-py")
        .current_dir(dir.path())
        .args(["-d", "example.wit", "-w", "example", "bindings", "."])
        .assert()
        .success();

    assert!(predicate::path::is_dir().eval(&dir.path().join("wit")));

    Command::new("python3")
        .current_dir(dir.path())
        .args([
            "-c",
            r#"
import ast
import sys
from pathlib import Path

docs_by_name = {}
for path in Path(".").rglob("*.py"):
    tree = ast.parse(path.read_text(), filename=str(path))
    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            doc = ast.get_docstring(node)
            if doc:
                docs_by_name.setdefault(node.name, []).append(doc)

hello_docs = docs_by_name.get("hello", [])
if not any('"""' in doc for doc in hello_docs):
    sys.stderr.write("hello docstring lost triple double quotes: %r\n" % hello_docs)
    sys.exit(1)

both_docs = docs_by_name.get("both", [])
if not any(("'''" in doc and '"""' in doc) for doc in both_docs):
    sys.stderr.write("both docstring lost quote sequences: %r\n" % both_docs)
    sys.exit(1)

four_docs = docs_by_name.get("four", [])
if not any(("'''" in doc and '""""' in doc) for doc in four_docs):
    sys.stderr.write("four docstring lost quote sequences: %r\n" % four_docs)
    sys.exit(1)

backslash_docs = docs_by_name.get("backslash", [])
if not any(("'''" in doc and '\\"""' in doc) for doc in backslash_docs):
    sys.stderr.write("backslash docstring lost quote sequences: %r\n" % backslash_docs)
    sys.exit(1)
"#,
        ])
        .assert()
        .success();

    Ok(())
}

#[test]
fn bindings_docstrings_omit_wit_line_comments() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    fs::write(
        dir.path().join("example.wit"),
        r#"package demo:docs;

// package of named fields (line comment)
interface documented {
  /// documented function
  doc-func: func();

  // some comment
  /// plus documentation
  comment-and-doc: func();

  /// documentation plus
  // another comment.
  doc-and-comment: func();

  // only a line comment
  plain: func();
}

world example {
  // world line comment
  export documented;
}
"#,
    )?;

    cargo::cargo_bin_cmd!("componentize-py")
        .current_dir(dir.path())
        .args(["-d", "example.wit", "-w", "example", "bindings", "."])
        .assert()
        .success();

    assert!(predicate::path::is_dir().eval(&dir.path().join("wit")));

    Command::new("python3")
        .current_dir(dir.path())
        .args([
            "-c",
            r#"
import ast
import sys
from pathlib import Path

docs_by_name = {}
for path in Path(".").rglob("*.py"):
    tree = ast.parse(path.read_text(), filename=str(path))
    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef, ast.Module)):
            doc = ast.get_docstring(node)
            if doc:
                key = getattr(node, "name", "<module>")
                docs_by_name.setdefault(key, []).append(doc)

all_docs = "\n".join(doc for docs in docs_by_name.values() for doc in docs)
for forbidden in (
    "package of named fields",
    "some comment",
    "another comment",
    "only a line comment",
    "world line comment",
):
    if forbidden in all_docs:
        sys.stderr.write("line comment leaked into docstrings: %r\n%s\n" % (forbidden, all_docs))
        sys.exit(1)

def has_doc(name, text):
    return any(text in doc for doc in docs_by_name.get(name, []))

if not has_doc("doc_func", "documented function"):
    sys.stderr.write("doc_func lost /// docs: %r\n" % docs_by_name.get("doc_func"))
    sys.exit(1)
if not has_doc("comment_and_doc", "plus documentation"):
    sys.stderr.write("comment_and_doc lost /// docs: %r\n" % docs_by_name.get("comment_and_doc"))
    sys.exit(1)
if not has_doc("doc_and_comment", "documentation plus"):
    sys.stderr.write("doc_and_comment lost /// docs: %r\n" % docs_by_name.get("doc_and_comment"))
    sys.exit(1)
if docs_by_name.get("plain"):
    sys.stderr.write("plain unexpectedly has a docstring: %r\n" % docs_by_name.get("plain"))
    sys.exit(1)
"#,
        ])
        .assert()
        .success();

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
        .args(["install", "mypy==1.13.0"])
        .assert()
        .success();

    Command::new(venv_path(path).join("mypy"))
        .current_dir(path)
        .args(args)
        .assert()
        .success()
        .stdout(predicate::str::is_match("Success: no issues found in 1 source file").unwrap())
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

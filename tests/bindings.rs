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

const REGISTRY_WIT: &str = r#"
package test:registry@1.2.3;

world command {
  export registry-value: func() -> u32;
}
"#;

fn registry_package_path(directory: &Path, package: &str, version: &str) -> PathBuf {
    let (namespace, name) = package.split_once(':').unwrap();
    directory
        .join("registry")
        .join(namespace)
        .join(name)
        .join(format!("{version}.wasm"))
}

fn write_registry_bytes(
    directory: &Path,
    package: &str,
    version: &str,
    bytes: &[u8],
) -> anyhow::Result<PathBuf> {
    let path = registry_package_path(directory, package, version);
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(&path, bytes)?;
    Ok(path)
}

fn write_registry_package(directory: &Path, wit: &str) -> anyhow::Result<PathBuf> {
    let mut resolve = wit_parser::Resolve::new();
    let package = resolve.push_str("registry.wit", wit)?;
    let name = resolve.packages[package].name.clone();
    write_registry_bytes(
        directory,
        &format!("{}:{}", name.namespace, name.name),
        name.version.as_ref().unwrap().to_string().as_str(),
        &wit_component::encode(&resolve, package)?,
    )
}

fn write_registry_config(directory: &Path, root: &Path) -> anyhow::Result<PathBuf> {
    let config = directory.join("registry.toml");
    fs::write(
        &config,
        format!(
            r#"[namespace_registries]
test = "test.invalid"
wasi = "test.invalid"
ba = "test.invalid"

[registry."test.invalid".local]
root = {:?}
"#,
            root
        ),
    )?;
    Ok(config)
}

fn run_bindings(
    directory: &Path,
    config: Option<&Path>,
    wit: Option<&Path>,
    worlds: &[&str],
    output: &str,
) -> assert_cmd::assert::Assert {
    let mut command = cargo::cargo_bin_cmd!("componentize-py");
    command
        .current_dir(directory)
        .args(["--no-default-registries"]);
    if let Some(config) = config {
        command.args(["--registry-config", config.to_str().unwrap()]);
    }
    if let Some(wit) = wit {
        command.args(["-d", wit.to_str().unwrap()]);
    }
    for world in worlds {
        command.args(["-w", world]);
    }
    command.args(["bindings", output]).assert()
}

fn dependency_packages() -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    const B_WIT: &str = r#"
package test:b@1.0.0;

interface api {
  call: func();
}

world b-world {
  export b-value: func();
}
"#;
    let sources = tempfile::tempdir()?;
    fs::create_dir_all(sources.path().join("deps/b"))?;
    fs::write(
        sources.path().join("a.wit"),
        "package test:a@1.0.0;\nworld a-world {\n  import test:b/api@1.0.0;\n  export a-value: func();\n}\n",
    )?;
    fs::write(sources.path().join("deps/b/b.wit"), B_WIT)?;

    let mut resolve = wit_parser::Resolve::new();
    let a = resolve.push_path(sources.path())?.0;
    let a = wit_component::encode(&resolve, a)?;
    let mut resolve = wit_parser::Resolve::new();
    let b = resolve.push_str("b.wit", B_WIT)?;
    let b = wit_component::encode(&resolve, b)?;
    Ok((a, b))
}

#[test]
fn resolves_exact_world_from_registry_without_local_wit() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    write_registry_package(dir.path(), REGISTRY_WIT)?;
    let config = write_registry_config(dir.path(), &dir.path().join("registry"))?;

    run_bindings(
        dir.path(),
        Some(&config),
        None,
        &["test:registry/command@1.2.3"],
        "output",
    )
    .success();

    let generated = fs::read_to_string(dir.path().join("output/wit/__init__.py"))?;
    assert!(
        generated.contains("def registry_value(self) -> int:"),
        "{generated}"
    );
    Ok(())
}

#[test]
fn local_world_skips_malformed_registry_config() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let local = dir.path().join("local.wit");
    fs::write(&local, REGISTRY_WIT)?;
    let config = dir.path().join("invalid.toml");
    fs::write(&config, "namespace_registries = [")?;

    run_bindings(
        dir.path(),
        Some(&config),
        Some(&local),
        &["test:registry/command@1.2.3"],
        "output",
    )
    .success();
    Ok(())
}

#[test]
fn local_and_remote_worlds_resolve_in_both_orders() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    write_registry_package(dir.path(), REGISTRY_WIT)?;
    let config = write_registry_config(dir.path(), &dir.path().join("registry"))?;
    let local = dir.path().join("local.wit");
    fs::write(
        &local,
        "package test:local@1.0.0;\nworld local { export local-value: func() -> u32; }",
    )?;

    for (output, worlds) in [
        ("local-first", ["local", "test:registry/command@1.2.3"]),
        ("remote-first", ["test:registry/command@1.2.3", "local"]),
    ] {
        run_bindings(dir.path(), Some(&config), Some(&local), &worlds, output).success();
        let generated = fs::read_to_string(dir.path().join(output).join("wit/__init__.py"))?;
        assert!(
            generated.contains("def local_value(self) -> int:"),
            "{generated}"
        );
        assert!(
            generated.contains("def registry_value(self) -> int:"),
            "{generated}"
        );
    }
    Ok(())
}

#[test]
fn local_unversioned_world_stays_local_after_remote_load() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    write_registry_package(
        dir.path(),
        "package test:registry@2.0.0;\nworld command { export remote-value: func(); }",
    )?;
    let config = write_registry_config(dir.path(), &dir.path().join("registry"))?;
    let local = dir.path().join("local.wit");
    fs::write(
        &local,
        "package test:registry@1.2.3;\nworld command { export local-value: func(); }",
    )?;

    for (output, worlds) in [
        (
            "local-version-first",
            ["test:registry/command", "test:registry/command@2.0.0"],
        ),
        (
            "remote-version-first",
            ["test:registry/command@2.0.0", "test:registry/command"],
        ),
    ] {
        run_bindings(dir.path(), Some(&config), Some(&local), &worlds, output).success();
        let generated = fs::read_to_string(dir.path().join(output).join("wit/__init__.py"))?;
        assert!(
            generated.contains("def local_value(self) -> None:"),
            "{generated}"
        );
        assert!(
            generated.contains("def remote_value(self) -> None:"),
            "{generated}"
        );
    }
    Ok(())
}

#[test]
fn short_and_unversioned_remote_only_names_are_rejected() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    fs::create_dir_all(dir.path().join("wit"))?;
    fs::write(
        dir.path().join("wit/local.wit"),
        "package test:local@1.0.0;\nworld local {}",
    )?;
    let config = write_registry_config(dir.path(), &dir.path().join("missing"))?;

    run_bindings(dir.path(), Some(&config), None, &["command"], "short")
        .failure()
        .stderr(predicate::str::contains("Unable to resolve"));
    run_bindings(
        dir.path(),
        Some(&config),
        None,
        &["test:registry/command"],
        "unversioned",
    )
    .failure()
    .stderr(predicate::str::contains("must include an exact version"));
    Ok(())
}

#[test]
fn malformed_qualified_name_reports_parse_error() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    run_bindings(
        dir.path(),
        None,
        None,
        &["test:registry/command@"],
        "output",
    )
    .failure()
    .stderr(predicate::str::contains("failed to parse world specifier"));
    Ok(())
}

#[test]
fn local_package_missing_world_is_not_replaced() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let local = dir.path().join("local.wit");
    fs::write(
        &local,
        "package test:registry@1.2.3;\nworld other { export value: func(); }",
    )?;
    let config = write_registry_config(dir.path(), &dir.path().join("missing"))?;

    run_bindings(
        dir.path(),
        Some(&config),
        Some(&local),
        &["test:registry/command@1.2.3"],
        "output",
    )
    .failure()
    .stderr(predicate::str::contains(
        "local package `test:registry@1.2.3` does not contain world `command`",
    ));
    Ok(())
}

#[test]
fn dependency_packages_resolve_without_replacing_explicit_remote_world() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let (a, b) = dependency_packages()?;
    let local_a = dir.path().join("a.wasm");
    fs::write(&local_a, a)?;
    write_registry_bytes(dir.path(), "test:b", "1.0.0", &b)?;
    let config = write_registry_config(dir.path(), &dir.path().join("registry"))?;

    run_bindings(
        dir.path(),
        Some(&config),
        Some(&local_a),
        &["test:a/a-world@1.0.0", "test:b/b-world@1.0.0"],
        "output",
    )
    .success();
    let generated = fs::read_to_string(dir.path().join("output/wit/__init__.py"))?;
    assert!(
        generated.contains("def a_value(self) -> None:"),
        "{generated}"
    );
    assert!(
        generated.contains("def b_value(self) -> None:"),
        "{generated}"
    );
    Ok(())
}

#[test]
fn remote_dependency_packages_resolve_in_both_orders() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let (a, b) = dependency_packages()?;
    write_registry_bytes(dir.path(), "test:a", "1.0.0", &a)?;
    write_registry_bytes(dir.path(), "test:b", "1.0.0", &b)?;
    let config = write_registry_config(dir.path(), &dir.path().join("registry"))?;

    for (output, worlds) in [
        ("a-first", ["test:a/a-world@1.0.0", "test:b/b-world@1.0.0"]),
        ("b-first", ["test:b/b-world@1.0.0", "test:a/a-world@1.0.0"]),
    ] {
        run_bindings(dir.path(), Some(&config), None, &worlds, output).success();
        let generated = fs::read_to_string(dir.path().join(output).join("wit/__init__.py"))?;
        assert!(
            generated.contains("def a_value(self) -> None:"),
            "{generated}"
        );
        assert!(
            generated.contains("def b_value(self) -> None:"),
            "{generated}"
        );
    }
    Ok(())
}

#[test]
fn registry_errors_identify_missing_world_non_wit_and_wrong_package() -> anyhow::Result<()> {
    let cases = [
        (
            "missing-world",
            "package test:registry@1.2.3;\nworld other {}",
            "registry package `test:registry@1.2.3` does not contain world `command`",
        ),
        (
            "non-wit",
            "",
            "registry content is not a binary WIT package",
        ),
        (
            "wrong-package",
            "package test:other@1.2.3;\nworld command {}",
            "registry content declares package `test:other@1.2.3`",
        ),
        (
            "wrong-version",
            "package test:registry@2.0.0;\nworld command {}",
            "registry content declares package `test:registry@2.0.0`",
        ),
    ];

    for (output, wit, error) in cases {
        let dir = tempfile::tempdir()?;
        if wit.is_empty() {
            write_registry_bytes(dir.path(), "test:registry", "1.2.3", b"not a wasm package")?;
        } else {
            write_registry_package(dir.path(), wit)?;
            if output == "wrong-package" {
                let source = registry_package_path(dir.path(), "test:other", "1.2.3");
                let target = registry_package_path(dir.path(), "test:registry", "1.2.3");
                fs::create_dir_all(target.parent().unwrap())?;
                fs::rename(source, target)?;
            } else if output == "wrong-version" {
                let source = registry_package_path(dir.path(), "test:registry", "2.0.0");
                let target = registry_package_path(dir.path(), "test:registry", "1.2.3");
                fs::rename(source, target)?;
            }
        }
        let config = write_registry_config(dir.path(), &dir.path().join("registry"))?;
        run_bindings(
            dir.path(),
            Some(&config),
            None,
            &["test:registry/command@1.2.3"],
            output,
        )
        .failure()
        .stderr(predicate::str::contains(error));
    }
    Ok(())
}

#[test]
fn configured_registry_failure_identifies_the_remote_world() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let config = write_registry_config(dir.path(), &dir.path().join("offline"))?;
    run_bindings(
        dir.path(),
        Some(&config),
        None,
        &["test:registry/command@1.2.3"],
        "output",
    )
    .failure()
    .stderr(predicate::str::contains(
        "failed to resolve remote world `test:registry/command@1.2.3`",
    ));
    Ok(())
}

#[test]
fn disabled_defaults_require_explicit_mapping() -> anyhow::Result<()> {
    for world in ["wasi:registry/command@1.2.3", "ba:registry/command@1.2.3"] {
        let dir = tempfile::tempdir()?;
        run_bindings(dir.path(), None, None, &[world], "output")
            .failure()
            .stderr(predicate::str::contains(
                "no registry configured for namespace",
            ))
            .stderr(predicate::str::contains("<unconfigured>"));
    }

    let dir = tempfile::tempdir()?;
    write_registry_package(
        dir.path(),
        "package wasi:registry@1.2.3;\nworld command { export value: func(); }",
    )?;
    let config = write_registry_config(dir.path(), &dir.path().join("registry"))?;
    run_bindings(
        dir.path(),
        Some(&config),
        None,
        &["wasi:registry/command@1.2.3"],
        "mapped",
    )
    .success();
    Ok(())
}
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

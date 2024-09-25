#![deny(warnings)]

use std::{fs, io::Cursor};

use anyhow::Context;
use tar::Archive;
use tempfile::TempDir;
use zstd::Decoder;

use crate::Library;

pub fn embedded_python_standard_library() -> TempDir {
        // Untar the embedded copy of the Python standard library into a temporary directory
        let stdlib = tempfile::tempdir().expect("could not create temp dirfor python stnadard lib");

        Archive::new(Decoder::new(Cursor::new(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/python-lib.tar.zst"
        )))).unwrap())
        .unpack(stdlib.path()).unwrap();

        return stdlib;
}

pub fn embedded_helper_utils() -> TempDir {
    // Untar the embedded copy of helper utilities into a temporary directory
    let bundled = tempfile::tempdir().expect("could not create tempdir for embedded helper utils");

    Archive::new(Decoder::new(Cursor::new(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/bundled.tar.zst"
    )))).unwrap())
    .unpack(bundled.path()).unwrap();

    return bundled;
}

pub fn bundle_libraries(library_path: Vec<(&str, Vec<std::path::PathBuf>)>) -> Vec<Library> {

    let mut libraries = vec![
        Library {
            name: "libcomponentize_py_runtime.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libcomponentize_py_runtime.so.zst"
            )))).unwrap(),
            dl_openable: false,
        },
        Library {
            name: "libpython3.12.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libpython3.12.so.zst"
            )))).unwrap(),
            dl_openable: false,
        },
        Library {
            name: "libc.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libc.so.zst"
            )))).unwrap(),
            dl_openable: false,
        },
        Library {
            name: "libwasi-emulated-mman.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libwasi-emulated-mman.so.zst"
            )))).unwrap(),
            dl_openable: false,
        },
        Library {
            name: "libwasi-emulated-process-clocks.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libwasi-emulated-process-clocks.so.zst"
            )))).unwrap(),
            dl_openable: false,
        },
        Library {
            name: "libwasi-emulated-getpid.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libwasi-emulated-getpid.so.zst"
            )))).unwrap(),
            dl_openable: false,
        },
        Library {
            name: "libwasi-emulated-signal.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libwasi-emulated-signal.so.zst"
            )))).unwrap(),
            dl_openable: false,
        },
        Library {
            name: "libc++.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libc++.so.zst"
            )))).unwrap(),
            dl_openable: false,
        },
        Library {
            name: "libc++abi.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libc++abi.so.zst"
            )))).unwrap(),
            dl_openable: false,
        }
    ];

    for (index, (path, libs)) in library_path.iter().enumerate() {
        for library in libs {
            let path = library
                .strip_prefix(path)
                .unwrap()
                .to_str()
                .context("non-UTF-8 path")
                .unwrap()
                .replace('\\', "/");

            libraries.push(Library {
                name: format!("/{index}/{path}"),
                module: fs::read(library).unwrap(),
                dl_openable: true,
            });
        }
    }

    return libraries;
}

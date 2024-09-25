#![deny(warnings)]

use std::{
    fs,
    io::{self, Cursor},
};

use anyhow::Context;
use tar::Archive;
use tempfile::TempDir;
use zstd::Decoder;

use crate::Library;

pub fn embedded_python_standard_library() -> Result<TempDir, io::Error> {
    // Untar the embedded copy of the Python standard library into a temporary directory
    let stdlib = tempfile::tempdir().expect("could not create temp dirfor python stnadard lib");

    Archive::new(Decoder::new(Cursor::new(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/python-lib.tar.zst"
    ))))?)
    .unpack(stdlib.path())
    .unwrap();

    Ok(stdlib)
}

pub fn embedded_helper_utils() -> Result<TempDir, io::Error> {
    // Untar the embedded copy of helper utilities into a temporary directory
    let bundled = tempfile::tempdir().expect("could not create tempdir for embedded helper utils");

    Archive::new(Decoder::new(Cursor::new(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/bundled.tar.zst"
    ))))?)
    .unpack(bundled.path())
    .unwrap();

    Ok(bundled)
}

pub fn bundle_libraries(
    library_path: Vec<(&str, Vec<std::path::PathBuf>)>,
) -> Result<Vec<Library>, io::Error> {
    let mut libraries = vec![
        Library {
            name: "libcomponentize_py_runtime.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libcomponentize_py_runtime.so.zst"
            ))))?,
            dl_openable: false,
        },
        Library {
            name: "libpython3.12.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libpython3.12.so.zst"
            ))))?,
            dl_openable: false,
        },
        Library {
            name: "libc.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libc.so.zst"
            ))))?,
            dl_openable: false,
        },
        Library {
            name: "libwasi-emulated-mman.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libwasi-emulated-mman.so.zst"
            ))))?,
            dl_openable: false,
        },
        Library {
            name: "libwasi-emulated-process-clocks.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libwasi-emulated-process-clocks.so.zst"
            ))))?,
            dl_openable: false,
        },
        Library {
            name: "libwasi-emulated-getpid.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libwasi-emulated-getpid.so.zst"
            ))))?,
            dl_openable: false,
        },
        Library {
            name: "libwasi-emulated-signal.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libwasi-emulated-signal.so.zst"
            ))))?,
            dl_openable: false,
        },
        Library {
            name: "libc++.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libc++.so.zst"
            ))))?,
            dl_openable: false,
        },
        Library {
            name: "libc++abi.so".into(),
            module: zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libc++abi.so.zst"
            ))))?,
            dl_openable: false,
        },
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

    Ok(libraries)
}

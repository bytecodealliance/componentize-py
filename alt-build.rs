//! This is an alternative `build.rs` file used only for publishing.
//!
//! When building from source, we use the normal `build.rs`, which generates a
//! bunch of artifacts as `.zst` files which are baked into the final binary.
//! However, the `libcomponentize_py_runtime_*.so.zst` files cannot be built as
//! part of a published crate due to the way Cargo works.  Therefore, the crate
//! we publish includes pre-build `.zst` files, which has the side effect of
//! making the build a lot faster.

#![deny(warnings)]

use std::{env, fs, path::PathBuf};

fn main() {
    let repo_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());

    let files = [
        "bundled.tar.zst",
        "libc++.so.zst",
        "libc++abi.so.zst",
        "libc.so.zst",
        "libcomponentize_py_runtime_async.so.zst",
        "libcomponentize_py_runtime_sync.so.zst",
        "libpython3.14.so.zst",
        "libwasi-emulated-getpid.so.zst",
        "libwasi-emulated-mman.so.zst",
        "libwasi-emulated-process-clocks.so.zst",
        "libwasi-emulated-signal.so.zst",
        "python-lib.tar.zst",
        "wasi_snapshot_preview1.reactor.wasm.zst",
    ];

    for file in files {
        fs::copy(repo_dir.join(file), out_dir.join(file)).unwrap();
    }

    // TODO: how can we detect `cargo test` and only run this in that case (or
    // more specifically, run it so it generates an empty file)?
    componentize_py_test_generator::generate().unwrap()
}

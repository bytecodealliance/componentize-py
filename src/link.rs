use std::io::Cursor;

use anyhow::Result;

use crate::Library;

pub fn link_libraries(libraries: &[Library]) -> Result<Vec<u8>> {
    let mut linker = wit_component::Linker::default()
        .validate(true)
        .use_built_in_libdl(true);

    for Library {
        name,
        module,
        dl_openable,
    } in libraries
    {
        linker = linker.library(name, module, *dl_openable)?;
    }

    linker = linker.adapter(
        "wasi_snapshot_preview1",
        &zstd::decode_all(Cursor::new(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/wasi_snapshot_preview1.reactor.wasm.zst"
        ))))?,
    )?;

    linker.encode().map_err(|e| anyhow::anyhow!(e))
}

use anyhow::Result;
use wasi_preview1_component_adapter_provider;

use crate::{command::WasiAdapter, Library};

pub fn link_libraries(libraries: &[Library], adapter: WasiAdapter) -> Result<Vec<u8>> {
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

    let adapter_module = match adapter {
        WasiAdapter::Proxy => {
            wasi_preview1_component_adapter_provider::WASI_SNAPSHOT_PREVIEW1_PROXY_ADAPTER
        }
        WasiAdapter::Reactor => {
            wasi_preview1_component_adapter_provider::WASI_SNAPSHOT_PREVIEW1_REACTOR_ADAPTER
        }
        _ => panic!("Adapater not supported"),
    };
    linker = linker.adapter("wasi_snapshot_preview1", adapter_module)?;

    linker.encode().map_err(|e| anyhow::anyhow!(e))
}

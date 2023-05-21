#![deny(warnings)]

use {
    super::ENGINE,
    anyhow::Result,
    async_trait::async_trait,
    wasi_preview2::WasiCtx,
    wasmtime::{
        component::{Component, Linker},
        Store,
    },
};

#[tokio::test]
async fn simple_export() -> Result<()> {
    wasmtime::component::bindgen!({
        path: "src/test/wit",
        world: "simple-export",
        async: true
    });

    let component = &super::make_component(
        include_str!("wit/simple-export.wit"),
        r#"
from simple_export import exports

class Exports(exports.Exports):
    def foo(v: int) -> int:
        return v + 3
"#,
    )?;

    let mut linker = Linker::new(&ENGINE);
    wasi_host::command::add_to_linker(&mut linker, |ctx| ctx)?;

    let mut store = Store::new(
        &ENGINE,
        wasmtime_wasi_preview2::WasiCtxBuilder::new()
            .inherit_stdout()
            .inherit_stderr()
            .build(),
    );

    let (instance, _) =
        SimpleExport::instantiate_async(&mut store, &Component::new(&ENGINE, component)?, &linker)
            .await?;

    assert_eq!(45, instance.exports.call_foo(&mut store, 42).await?);

    Ok(())
}

#[tokio::test]
async fn simple_import_and_export() -> Result<()> {
    wasmtime::component::bindgen!({
        path: "src/test/wit",
        world: "simple-import-and-export",
        async: true
    });

    struct Host {
        wasi: WasiCtx,
    }

    #[async_trait]
    impl imports::Host for Host {
        async fn foo(&mut self, v: u32) -> Result<u32> {
            Ok(v + 2)
        }
    }

    let component = &super::make_component(
        include_str!("wit/simple-import-and-export.wit"),
        r#"
from simple_import_and_export import exports
from simple_import_and_export.imports import imports

class Exports(exports.Exports):
    def foo(v: int) -> int:
        return imports.foo(v) + 3
"#,
    )?;

    let mut linker = Linker::<Host>::new(&ENGINE);
    wasi_host::command::add_to_linker(&mut linker, |host| &mut host.wasi)?;
    imports::add_to_linker(&mut linker, |host| host)?;

    let mut store = Store::new(
        &ENGINE,
        Host {
            wasi: wasmtime_wasi_preview2::WasiCtxBuilder::new()
                .inherit_stdout()
                .inherit_stderr()
                .build(),
        },
    );

    let (instance, _) = SimpleImportAndExport::instantiate_async(
        &mut store,
        &Component::new(&ENGINE, component)?,
        &linker,
    )
    .await?;

    assert_eq!(47, instance.exports.call_foo(&mut store, 42).await?);

    Ok(())
}

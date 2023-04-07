#[cfg(test)]
mod tests {
    use {
        anyhow::{Error, Result},
        async_trait::async_trait,
        std::{fs, process::Command, sync::Once},
        wasi_preview2::WasiCtx,
        wasmtime::{
            component::{Component, Linker},
            Config, Engine, Store,
        },
    };

    fn make_component(wit: &str, python: &str) -> Result<Vec<u8>> {
        static ONCE: Once = Once::new();

        let once = || {
            assert!(Command::new("cargo")
                .current_dir("..")
                .arg("build")
                .arg("--release")
                .status()?
                .success());

            Ok::<(), Error>(())
        };

        ONCE.call_once(|| once().unwrap());

        let tempdir = tempfile::tempdir()?;
        fs::write(tempdir.path().join("app.wit"), wit)?;
        fs::write(tempdir.path().join("app.py"), python)?;

        assert!(Command::new("../target/release/componentize-py")
            .arg("app")
            .arg("--wit-path")
            .arg(tempdir.path().join("app.wit"))
            .arg("--python-path")
            .arg(tempdir.path())
            .arg("--output")
            .arg(tempdir.path().join("app.wasm"))
            .arg("--quiet")
            .status()?
            .success());

        Ok(fs::read(tempdir.path().join("app.wasm"))?)
    }

    #[tokio::test]
    async fn simple_export() -> Result<()> {
        wasmtime::component::bindgen!({
            path: "wit",
            world: "simple-export",
            async: true
        });

        let component = &make_component(
            include_str!("../wit/simple-export.wit"),
            r#"
def exports_foo(v):
    return v + 3
"#,
        )?;

        let mut config = Config::new();
        config.async_support(true);
        config.wasm_component_model(true);

        let engine = &Engine::new(&config)?;
        let mut linker = Linker::new(engine);
        wasi_host::command::add_to_linker(&mut linker, |ctx| ctx)?;

        let mut store = Store::new(
            engine,
            wasmtime_wasi_preview2::WasiCtxBuilder::new()
                .inherit_stdout()
                .inherit_stderr()
                .build(),
        );

        let (instance, _) = SimpleExport::instantiate_async(
            &mut store,
            &Component::new(engine, component)?,
            &linker,
        )
        .await?;

        assert_eq!(45, instance.exports.call_foo(&mut store, 42).await?);

        Ok(())
    }

    #[tokio::test]
    async fn simple_import_and_export() -> Result<()> {
        wasmtime::component::bindgen!({
            path: "wit",
            world: "simple-import-and-export",
            async: true
        });

        struct Host;

        #[async_trait]
        impl imports::Host for Host {
            async fn foo(&mut self, v: u32) -> Result<u32> {
                Ok(v + 2)
            }
        }

        struct Data {
            host: Host,
            wasi: WasiCtx,
        }

        let component = &make_component(
            include_str!("../wit/simple-import-and-export.wit"),
            r#"
import imports

def exports_foo(v):
    return imports.foo(v) + 3
"#,
        )?;

        let mut config = Config::new();
        config.async_support(true);
        config.wasm_component_model(true);

        let engine = &Engine::new(&config)?;
        let mut linker = Linker::<Data>::new(engine);
        wasi_host::command::add_to_linker(&mut linker, |data| &mut data.wasi)?;
        imports::add_to_linker(&mut linker, |data| &mut data.host)?;

        let mut store = Store::new(
            engine,
            Data {
                host: Host,
                wasi: wasmtime_wasi_preview2::WasiCtxBuilder::new()
                    .inherit_stdout()
                    .inherit_stderr()
                    .build(),
            },
        );

        let (instance, _) = SimpleImportAndExport::instantiate_async(
            &mut store,
            &Component::new(engine, component)?,
            &linker,
        )
        .await?;

        assert_eq!(47, instance.exports.call_foo(&mut store, 42).await?);

        Ok(())
    }
}

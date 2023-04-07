#[cfg(test)]
mod tests {
    use {
        anyhow::{Error, Result},
        std::{fs, process::Command, sync::Once},
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
            .status()?
            .success());

        Ok(fs::read(tempdir.path().join("app.wasm"))?)
    }

    wasmtime::component::bindgen!({
        path: "wit",
        world: "simple-export",
        async: true
    });

    #[tokio::test]
    async fn simple_export() -> Result<()> {
        let component = &make_component(
            include_str!("../wit/simple-export.wit"),
            r#"
def exports_bar(v):
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

        let (simple_export, _) = SimpleExport::instantiate_async(
            &mut store,
            &Component::new(engine, component)?,
            &linker,
        )
        .await?;

        assert_eq!(45, simple_export.exports.call_bar(&mut store, 42).await?);

        Ok(())
    }
}

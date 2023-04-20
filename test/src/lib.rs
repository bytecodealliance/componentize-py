#![deny(warnings)]
#[cfg(test)]
mod tests {
    use {
        anyhow::{Error, Result},
        async_trait::async_trait,
        once_cell::sync::Lazy,
        proptest::{
            prelude::Strategy,
            test_runner::{self, TestRng, TestRunner},
        },
        std::{env, fs, process::Command, sync::Once},
        tokio::runtime::Runtime,
        wasi_preview2::WasiCtx,
        wasmtime::{
            component::{Component, InstancePre, Linker},
            Config, Engine, Store,
        },
    };

    mod echoes;
    #[allow(warnings)]
    mod echoes_generated;

    fn get_seed() -> Result<[u8; 32]> {
        let seed =
            <[u8; 32]>::try_from(hex::decode(env!("COMPONENTIZE_PY_TEST_SEED"))?.as_slice())?;

        eprintln!(
            "using seed {} (set COMPONENTIZE_PY_TEST_SEED env var to override)",
            hex::encode(seed)
        );

        Ok(seed)
    }

    static SEED: Lazy<[u8; 32]> = Lazy::new(|| get_seed().unwrap());

    static ENGINE: Lazy<Engine> = Lazy::new(|| {
        let mut config = Config::new();
        config.async_support(true);
        config.wasm_component_model(true);

        Engine::new(&config).unwrap()
    });

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

    #[derive(Debug, Copy, Clone)]
    struct MyFloat32(f32);

    impl PartialEq<MyFloat32> for MyFloat32 {
        fn eq(&self, other: &Self) -> bool {
            (self.0.is_nan() && other.0.is_nan()) || (self.0 == other.0)
        }
    }

    #[derive(Debug, Copy, Clone)]
    struct MyFloat64(f64);

    impl PartialEq<MyFloat64> for MyFloat64 {
        fn eq(&self, other: &Self) -> bool {
            (self.0.is_nan() && other.0.is_nan()) || (self.0 == other.0)
        }
    }

    #[async_trait]
    trait Host {
        type World;

        fn new(wasi: WasiCtx) -> Self;

        fn add_to_linker(linker: &mut Linker<Self>) -> Result<()>
        where
            Self: Sized;

        async fn instantiate_pre(
            store: &mut Store<Self>,
            pre: &InstancePre<Self>,
        ) -> Result<Self::World>
        where
            Self: Sized;
    }

    struct Tester<H> {
        pre: InstancePre<H>,
        seed: [u8; 32],
    }

    impl<H: Host> Tester<H> {
        fn new(wit: &str, guest_code: &str, seed: [u8; 32]) -> Result<Self> {
            let component = &make_component(wit, guest_code)?;
            let mut linker = Linker::<H>::new(&ENGINE);
            H::add_to_linker(&mut linker)?;
            Ok(Self {
                pre: linker.instantiate_pre(&Component::new(&ENGINE, component)?)?,
                seed,
            })
        }

        pub fn test<S: Strategy>(
            &self,
            strategy: &S,
            test: impl Fn(S::Value, &H::World, &mut Store<H>, &Runtime) -> Result<()>,
        ) -> Result<()>
        where
            S::Value: PartialEq<S::Value> + Clone + Send + Sync + 'static,
        {
            let runtime = Runtime::new()?;
            let config = test_runner::Config::default();
            let algorithm = config.rng_algorithm;
            let mut runner =
                TestRunner::new_with_rng(config, TestRng::from_seed(algorithm, &self.seed));

            Ok(runner.run(strategy, move |v| {
                let mut store = Store::new(
                    &ENGINE,
                    H::new(
                        wasmtime_wasi_preview2::WasiCtxBuilder::new()
                            .inherit_stdout()
                            .inherit_stderr()
                            .build(),
                    ),
                );

                let instance = runtime
                    .block_on(H::instantiate_pre(&mut store, &self.pre))
                    .unwrap();

                test(v, &instance, &mut store, &runtime).unwrap();
                Ok(())
            })?)
        }

        pub fn all_eq<S: Strategy>(
            &self,
            strategy: &S,
            echo: impl Fn(S::Value, &H::World, &mut Store<H>, &Runtime) -> Result<S::Value>,
        ) -> Result<()>
        where
            S::Value: PartialEq<S::Value> + Clone + Send + Sync + 'static,
        {
            self.test(strategy, |v, instance, store, runtime| {
                assert_eq!(v, echo(v.clone(), instance, store, runtime)?);
                Ok(())
            })
        }
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

        let mut linker = Linker::new(&ENGINE);
        wasi_host::command::add_to_linker(&mut linker, |ctx| ctx)?;

        let mut store = Store::new(
            &ENGINE,
            wasmtime_wasi_preview2::WasiCtxBuilder::new()
                .inherit_stdout()
                .inherit_stderr()
                .build(),
        );

        let (instance, _) = SimpleExport::instantiate_async(
            &mut store,
            &Component::new(&ENGINE, component)?,
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

        struct Host {
            wasi: WasiCtx,
        }

        #[async_trait]
        impl imports::Host for Host {
            async fn foo(&mut self, v: u32) -> Result<u32> {
                Ok(v + 2)
            }
        }

        let component = &make_component(
            include_str!("../wit/simple-import-and-export.wit"),
            r#"
import imports

def exports_foo(v):
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
}

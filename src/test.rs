use {
    anyhow::{anyhow, Result},
    async_trait::async_trait,
    once_cell::sync::Lazy,
    proptest::{
        prelude::Strategy,
        test_runner::{self, TestRng, TestRunner},
    },
    std::{env, fs},
    tokio::runtime::Runtime,
    wasi_preview2::WasiCtx,
    wasmtime::{
        component::{Component, InstancePre, Linker},
        Config, Engine, Store,
    },
};

mod echoes;
mod echoes_generated;
mod simple;

fn get_seed() -> Result<[u8; 32]> {
    let seed = <[u8; 32]>::try_from(hex::decode(env!("COMPONENTIZE_PY_TEST_SEED"))?.as_slice())?;

    eprintln!(
        "using seed {} (set COMPONENTIZE_PY_TEST_SEED env var to override)",
        hex::encode(seed)
    );

    Ok(seed)
}

pub static SEED: Lazy<[u8; 32]> = Lazy::new(|| get_seed().unwrap());

pub static ENGINE: Lazy<Engine> = Lazy::new(|| {
    let mut config = Config::new();
    config.async_support(true);
    config.wasm_component_model(true);

    Engine::new(&config).unwrap()
});

pub fn make_component(wit: &str, python: &str) -> Result<Vec<u8>> {
    let tempdir = tempfile::tempdir()?;
    fs::write(tempdir.path().join("app.wit"), wit)?;
    fs::write(tempdir.path().join("app.py"), python)?;

    crate::componentize(
        &tempdir.path().join("app.wit"),
        None,
        tempdir
            .path()
            .to_str()
            .ok_or_else(|| anyhow!("unable to parse temporary directory path as UTF-8"))?,
        "app",
        false,
        &tempdir.path().join("app.wasm"),
    )?;

    Ok(fs::read(tempdir.path().join("app.wasm"))?)
}

#[derive(Debug, Copy, Clone)]
pub struct MyFloat32(pub f32);

impl PartialEq<MyFloat32> for MyFloat32 {
    fn eq(&self, other: &Self) -> bool {
        (self.0.is_nan() && other.0.is_nan()) || (self.0 == other.0)
    }
}

#[derive(Debug, Copy, Clone)]
pub struct MyFloat64(pub f64);

impl PartialEq<MyFloat64> for MyFloat64 {
    fn eq(&self, other: &Self) -> bool {
        (self.0.is_nan() && other.0.is_nan()) || (self.0 == other.0)
    }
}

#[async_trait]
pub trait Host {
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

pub struct Tester<H> {
    pre: InstancePre<H>,
    seed: [u8; 32],
}

impl<H: Host> Tester<H> {
    pub fn new(wit: &str, guest_code: &str, seed: [u8; 32]) -> Result<Self> {
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

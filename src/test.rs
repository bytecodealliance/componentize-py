#![deny(warnings)]

use {
    crate::Ctx,
    anyhow::{anyhow, Result},
    async_trait::async_trait,
    once_cell::sync::Lazy,
    proptest::{
        prelude::Strategy,
        test_runner::{self, TestRng, TestRunner},
    },
    std::{env, fs, marker::PhantomData},
    tokio::runtime::Runtime,
    wasmtime::{
        component::{Component, Instance, InstancePre, Linker, ResourceTable},
        Config, Engine, Store,
    },
    wasmtime_wasi::preview2::WasiCtxBuilder,
};

mod echoes;
mod echoes_generated;
mod tests;

fn get_seed() -> Result<[u8; 32]> {
    let seed = <[u8; 32]>::try_from(hex::decode(env!("COMPONENTIZE_PY_TEST_SEED"))?.as_slice())?;

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

#[allow(clippy::type_complexity)]
async fn make_component(
    wit: &str,
    guest_code: &[(&str, &str)],
    add_to_linker: Option<&dyn Fn(&mut Linker<Ctx>) -> Result<()>>,
) -> Result<Vec<u8>> {
    let tempdir = tempfile::tempdir()?;
    fs::write(tempdir.path().join("app.wit"), wit)?;

    for (name, content) in guest_code {
        let path = tempdir.path().join(name);
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, content)?;
    }

    crate::componentize(
        Some(&tempdir.path().join("app.wit")),
        None,
        &[tempdir
            .path()
            .to_str()
            .ok_or_else(|| anyhow!("unable to parse temporary directory path as UTF-8"))?],
        "app",
        &tempdir.path().join("app.wasm"),
        add_to_linker,
    )
    .await?;

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

    fn add_to_linker(linker: &mut Linker<Ctx>) -> Result<()>;

    async fn instantiate_pre(
        store: &mut Store<Ctx>,
        pre: &InstancePre<Ctx>,
    ) -> Result<(Self::World, Instance)>;
}

struct Tester<H> {
    pre: InstancePre<Ctx>,
    seed: [u8; 32],
    _phantom: PhantomData<H>,
}

impl<H: Host> Tester<H> {
    fn new(wit: &str, guest_code: &[(&str, &str)], seed: [u8; 32]) -> Result<Self> {
        // TODO: create two versions of the component -- one with and one without an `add_to_linker` -- and run
        // each test on each component in the `test` method (but probably not in the `proptest` method, since that
        // would slow it down a lot).  This will help exercise the stub mechanism when pre-initializing.
        let component =
            &Runtime::new()?.block_on(make_component(wit, guest_code, Some(&H::add_to_linker)))?;
        let mut linker = Linker::<Ctx>::new(&ENGINE);
        H::add_to_linker(&mut linker)?;
        Ok(Self {
            pre: linker.instantiate_pre(&Component::new(&ENGINE, component)?)?,
            seed,
            _phantom: PhantomData,
        })
    }

    fn test(
        &self,
        test: impl Fn(&H::World, &mut Store<Ctx>, &Runtime) -> Result<()>,
    ) -> Result<()> {
        let runtime = Runtime::new()?;

        let mut store = runtime.block_on(async {
            let table = ResourceTable::new();
            let wasi = WasiCtxBuilder::new()
                .inherit_stdout()
                .inherit_stderr()
                .build();

            Store::new(&ENGINE, Ctx { wasi, table })
        });

        let (world, _) = runtime
            .block_on(H::instantiate_pre(&mut store, &self.pre))
            .unwrap();

        test(&world, &mut store, &runtime)
    }

    fn proptest<S: Strategy>(
        &self,
        strategy: &S,
        test: impl Fn(S::Value, &H::World, &mut Store<Ctx>, &Runtime) -> Result<()>,
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
            let mut store = runtime.block_on(async {
                let table = ResourceTable::new();
                let wasi = WasiCtxBuilder::new()
                    .inherit_stdout()
                    .inherit_stderr()
                    .build();

                Store::new(&ENGINE, Ctx { wasi, table })
            });

            let (world, _) = runtime
                .block_on(H::instantiate_pre(&mut store, &self.pre))
                .unwrap();

            test(v, &world, &mut store, &runtime).unwrap();
            Ok(())
        })?)
    }

    fn all_eq<S: Strategy>(
        &self,
        strategy: &S,
        echo: impl Fn(S::Value, &H::World, &mut Store<Ctx>, &Runtime) -> Result<S::Value>,
    ) -> Result<()>
    where
        S::Value: PartialEq<S::Value> + Clone + Send + Sync + 'static,
    {
        self.proptest(strategy, |v, world, store, runtime| {
            assert_eq!(v, echo(v.clone(), world, store, runtime)?);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use {
        anyhow::{Error, Result},
        async_trait::async_trait,
        once_cell::sync::Lazy,
        proptest::{prelude::Strategy, test_runner::TestRunner},
        std::{cell::RefCell, fs, process::Command, sync::Once},
        tokio::runtime::Runtime,
        wasi_preview2::WasiCtx,
        wasmtime::{
            component::{Component, InstancePre, Linker},
            Config, Engine, Store,
        },
    };

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

    mod echoes {
        use super::*;

        wasmtime::component::bindgen!({
            path: "wit",
            world: "echoes",
            async: true
        });

        pub struct Host {
            wasi: WasiCtx,
        }

        #[async_trait]
        impl imports::Host for Host {
            async fn echo_bool(&mut self, v: bool) -> Result<bool> {
                Ok(v)
            }

            async fn echo_u8(&mut self, v: u8) -> Result<u8> {
                Ok(v)
            }

            async fn echo_s8(&mut self, v: i8) -> Result<i8> {
                Ok(v)
            }

            async fn echo_u16(&mut self, v: u16) -> Result<u16> {
                Ok(v)
            }

            async fn echo_s16(&mut self, v: i16) -> Result<i16> {
                Ok(v)
            }

            async fn echo_u32(&mut self, v: u32) -> Result<u32> {
                Ok(v)
            }

            async fn echo_s32(&mut self, v: i32) -> Result<i32> {
                Ok(v)
            }

            async fn echo_char(&mut self, v: char) -> Result<char> {
                Ok(v)
            }

            async fn echo_u64(&mut self, v: u64) -> Result<u64> {
                Ok(v)
            }

            async fn echo_s64(&mut self, v: i64) -> Result<i64> {
                Ok(v)
            }

            async fn echo_float32(&mut self, v: f32) -> Result<f32> {
                Ok(v)
            }

            async fn echo_float64(&mut self, v: f64) -> Result<f64> {
                Ok(v)
            }

            async fn echo_string(&mut self, v: String) -> Result<String> {
                Ok(v)
            }

            async fn echo_list_bool(&mut self, v: Vec<bool>) -> Result<Vec<bool>> {
                Ok(v)
            }

            async fn echo_list_u8(&mut self, v: Vec<u8>) -> Result<Vec<u8>> {
                Ok(v)
            }

            async fn echo_list_s8(&mut self, v: Vec<i8>) -> Result<Vec<i8>> {
                Ok(v)
            }

            async fn echo_list_u16(&mut self, v: Vec<u16>) -> Result<Vec<u16>> {
                Ok(v)
            }

            async fn echo_list_s16(&mut self, v: Vec<i16>) -> Result<Vec<i16>> {
                Ok(v)
            }

            async fn echo_list_u32(&mut self, v: Vec<u32>) -> Result<Vec<u32>> {
                Ok(v)
            }

            async fn echo_list_s32(&mut self, v: Vec<i32>) -> Result<Vec<i32>> {
                Ok(v)
            }

            async fn echo_list_char(&mut self, v: Vec<char>) -> Result<Vec<char>> {
                Ok(v)
            }

            async fn echo_list_u64(&mut self, v: Vec<u64>) -> Result<Vec<u64>> {
                Ok(v)
            }

            async fn echo_list_s64(&mut self, v: Vec<i64>) -> Result<Vec<i64>> {
                Ok(v)
            }

            async fn echo_list_float32(&mut self, v: Vec<f32>) -> Result<Vec<f32>> {
                Ok(v)
            }

            async fn echo_list_float64(&mut self, v: Vec<f64>) -> Result<Vec<f64>> {
                Ok(v)
            }

            async fn echo_list_string(&mut self, v: Vec<String>) -> Result<Vec<String>> {
                Ok(v)
            }

            async fn echo_list_list_u8(&mut self, v: Vec<Vec<u8>>) -> Result<Vec<Vec<u8>>> {
                Ok(v)
            }

            async fn echo_list_list_list_u8(
                &mut self,
                v: Vec<Vec<Vec<u8>>>,
            ) -> Result<Vec<Vec<Vec<u8>>>> {
                Ok(v)
            }

            async fn echo_many(
                &mut self,
                v1: bool,
                v2: u8,
                v3: u16,
                v4: u32,
                v5: u64,
                v6: i8,
                v7: i16,
                v8: i32,
                v9: i64,
                v10: f32,
                v11: f64,
                v12: char,
                v13: String,
                v14: Vec<bool>,
                v15: Vec<u8>,
                v16: Vec<u16>,
            ) -> Result<(
                bool,
                u8,
                u16,
                u32,
                u64,
                i8,
                i16,
                i32,
                i64,
                f32,
                f64,
                char,
                String,
                Vec<bool>,
                Vec<u8>,
                Vec<u16>,
            )> {
                Ok((
                    v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, v11, v12, v13, v14, v15, v16,
                ))
            }
        }

        fn instance_pre() -> Result<InstancePre<Host>> {
            let component = &make_component(
                include_str!("../wit/echoes.wit"),
                r#"
import imports

def exports_echo_bool(v):
    return imports.echo_bool(v)

def exports_echo_u8(v):
    return imports.echo_u8(v)

def exports_echo_s8(v):
    return imports.echo_s8(v)

def exports_echo_u16(v):
    return imports.echo_u16(v)

def exports_echo_s16(v):
    return imports.echo_s16(v)

def exports_echo_u32(v):
    return imports.echo_u32(v)

def exports_echo_s32(v):
    return imports.echo_s32(v)

def exports_echo_char(v):
    return imports.echo_char(v)

def exports_echo_u64(v):
    return imports.echo_u64(v)

def exports_echo_s64(v):
    return imports.echo_s64(v)

def exports_echo_float32(v):
    return imports.echo_float32(v)

def exports_echo_float64(v):
    return imports.echo_float64(v)

def exports_echo_string(v):
    return imports.echo_string(v)

def exports_echo_list_bool(v):
    return imports.echo_list_bool(v)

def exports_echo_list_u8(v):
    return imports.echo_list_u8(v)

def exports_echo_list_s8(v):
    return imports.echo_list_s8(v)

def exports_echo_list_u16(v):
    return imports.echo_list_u16(v)

def exports_echo_list_s16(v):
    return imports.echo_list_s16(v)

def exports_echo_list_u32(v):
    return imports.echo_list_u32(v)

def exports_echo_list_s32(v):
    return imports.echo_list_s32(v)

def exports_echo_list_char(v):
    return imports.echo_list_char(v)

def exports_echo_list_u64(v):
    return imports.echo_list_u64(v)

def exports_echo_list_s64(v):
    return imports.echo_list_s64(v)

def exports_echo_list_float32(v):
    return imports.echo_list_float32(v)

def exports_echo_list_float64(v):
    return imports.echo_list_float64(v)

def exports_echo_list_string(v):
    return imports.echo_list_string(v)

def exports_echo_list_list_u8(v):
    return imports.echo_list_list_u8(v)

def exports_echo_list_list_list_u8(v):
    return imports.echo_list_list_list_u8(v)

def exports_echo_many(v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, v11, v12, v13, v14, v15, v16):
    return imports.echo_many(v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, v11, v12, v13, v14, v15, v16)
"#,
            )?;

            let mut linker = Linker::<Host>::new(&ENGINE);
            wasi_host::command::add_to_linker(&mut linker, |host| &mut host.wasi)?;
            imports::add_to_linker(&mut linker, |host| host)?;
            linker.instantiate_pre(&Component::new(&ENGINE, component)?)
        }

        pub static INSTANCE_PRE: Lazy<InstancePre<Host>> = Lazy::new(|| instance_pre().unwrap());

        pub fn store() -> Store<Host> {
            Store::new(
                &ENGINE,
                Host {
                    wasi: wasmtime_wasi_preview2::WasiCtxBuilder::new()
                        .inherit_stdout()
                        .inherit_stderr()
                        .build(),
                },
            )
        }

        pub fn test<S: Strategy>(
            strategy: &S,
            test: impl Fn(S::Value, &Echoes, &mut Store<Host>, &Runtime) -> Result<()>,
        ) -> Result<()>
        where
            S::Value: PartialEq<S::Value> + Clone + Send + Sync + 'static,
        {
            let runtime = Runtime::new()?;
            let mut store = store();
            let (instance, _) =
                runtime.block_on(Echoes::instantiate_pre(&mut store, &INSTANCE_PRE))?;
            let store = RefCell::new(store);

            Ok(TestRunner::default().run(strategy, move |v| {
                test(v.clone(), &instance, &mut store.borrow_mut(), &runtime).unwrap();
                Ok(())
            })?)
        }

        pub fn all_eq<S: Strategy>(
            strategy: &S,
            echo: impl Fn(S::Value, &Echoes, &mut Store<Host>, &Runtime) -> Result<S::Value>,
        ) -> Result<()>
        where
            S::Value: PartialEq<S::Value> + Clone + Send + Sync + 'static,
        {
            test(strategy, |v, instance, store, runtime| {
                assert_eq!(v, echo(v.clone(), instance, store, runtime)?);
                Ok(())
            })
        }
    }

    #[test]
    fn bools() -> Result<()> {
        echoes::all_eq(&proptest::bool::ANY, |v, instance, store, runtime| {
            runtime.block_on(instance.exports().call_echo_bool(store, v))
        })
    }

    #[test]
    fn u8s() -> Result<()> {
        echoes::all_eq(&proptest::num::u8::ANY, |v, instance, store, runtime| {
            runtime.block_on(instance.exports().call_echo_u8(store, v))
        })
    }

    #[test]
    fn s8s() -> Result<()> {
        echoes::all_eq(&proptest::num::i8::ANY, |v, instance, store, runtime| {
            runtime.block_on(instance.exports().call_echo_s8(store, v))
        })
    }

    #[test]
    fn u16s() -> Result<()> {
        echoes::all_eq(&proptest::num::u16::ANY, |v, instance, store, runtime| {
            runtime.block_on(instance.exports().call_echo_u16(store, v))
        })
    }

    #[test]
    fn s16s() -> Result<()> {
        echoes::all_eq(&proptest::num::i16::ANY, |v, instance, store, runtime| {
            runtime.block_on(instance.exports().call_echo_s16(store, v))
        })
    }

    #[test]
    fn u32s() -> Result<()> {
        echoes::all_eq(&proptest::num::u32::ANY, |v, instance, store, runtime| {
            runtime.block_on(instance.exports().call_echo_u32(store, v))
        })
    }

    #[test]
    fn s32s() -> Result<()> {
        echoes::all_eq(&proptest::num::i32::ANY, |v, instance, store, runtime| {
            runtime.block_on(instance.exports().call_echo_s32(store, v))
        })
    }

    #[test]
    fn u64s() -> Result<()> {
        echoes::all_eq(&proptest::num::u64::ANY, |v, instance, store, runtime| {
            runtime.block_on(instance.exports().call_echo_u64(store, v))
        })
    }

    #[test]
    fn s64s() -> Result<()> {
        echoes::all_eq(&proptest::num::i64::ANY, |v, instance, store, runtime| {
            runtime.block_on(instance.exports().call_echo_s64(store, v))
        })
    }

    #[test]
    fn chars() -> Result<()> {
        echoes::all_eq(&proptest::char::any(), |v, instance, store, runtime| {
            runtime.block_on(instance.exports().call_echo_char(store, v))
        })
    }

    #[test]
    fn float32s() -> Result<()> {
        echoes::test(&proptest::num::f32::ANY, |v, instance, store, runtime| {
            let result = runtime.block_on(instance.exports().call_echo_float32(store, v))?;
            assert!((result.is_nan() && v.is_nan()) || result == v);
            Ok(())
        })
    }

    #[test]
    fn float64s() -> Result<()> {
        echoes::test(&proptest::num::f64::ANY, |v, instance, store, runtime| {
            let result = runtime.block_on(instance.exports().call_echo_float64(store, v))?;
            assert!((result.is_nan() && v.is_nan()) || result == v);
            Ok(())
        })
    }
}

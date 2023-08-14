#![deny(warnings)]

use {
    anyhow::{anyhow, Context, Result},
    async_trait::async_trait,
    bytes::Bytes,
    component_init::Invoker,
    exports::exports::RawUnionType,
    futures::future::FutureExt,
    heck::ToSnakeCase,
    std::{
        env,
        fs::{self, File},
        hash::{Hash, Hasher},
        io::Cursor,
        mem,
        path::{Path, PathBuf},
        str,
    },
    summary::Summary,
    tar::Archive,
    wasmtime::{
        component::{Component, Instance, Linker},
        Config, Engine, Store,
    },
    wasmtime_wasi::{
        preview2::{
            command as wasi_command,
            pipe::{MemoryInputPipe, MemoryOutputPipe},
            DirPerms, FilePerms, Table, WasiCtx, WasiCtxBuilder, WasiView,
        },
        Dir,
    },
    wit_parser::{Resolve, UnresolvedPackage, WorldId, WorldItem, WorldKey},
    zstd::Decoder,
};

mod abi;
mod bindgen;
mod bindings;
pub mod command;
#[cfg(feature = "pyo3")]
mod python;
mod summary;
#[cfg(test)]
mod test;
mod util;

static NATIVE_EXTENSION_SUFFIX: &str = ".cpython-311-wasm32-wasi.so";

wasmtime::component::bindgen!({
    path: "wit/init.wit",
    world: "init",
    async: true
});

impl Hash for RawUnionType {
    fn hash<H: Hasher>(&self, state: &mut H) {
        mem::discriminant(self).hash(state)
    }
}

#[cfg(unix)]
const NATIVE_PATH_DELIMITER: char = ':';

#[cfg(windows)]
const NATIVE_PATH_DELIMITER: char = ';';

pub struct Ctx {
    wasi: WasiCtx,
    table: Table,
}

impl WasiView for Ctx {
    fn ctx(&self) -> &WasiCtx {
        &self.wasi
    }
    fn ctx_mut(&mut self) -> &mut WasiCtx {
        &mut self.wasi
    }
    fn table(&self) -> &Table {
        &self.table
    }
    fn table_mut(&mut self) -> &mut Table {
        &mut self.table
    }
}

struct MyInvoker {
    store: Store<Ctx>,
    instance: Instance,
}

#[async_trait]
impl Invoker for MyInvoker {
    async fn call_s32(&mut self, function: &str) -> Result<i32> {
        let func = self
            .instance
            .exports(&mut self.store)
            .root()
            .typed_func::<(), (i32,)>(function)?;
        let result = func.call_async(&mut self.store, ()).await?.0;
        func.post_return_async(&mut self.store).await?;
        Ok(result)
    }

    async fn call_s64(&mut self, function: &str) -> Result<i64> {
        let func = self
            .instance
            .exports(&mut self.store)
            .root()
            .typed_func::<(), (i64,)>(function)?;
        let result = func.call_async(&mut self.store, ()).await?.0;
        func.post_return_async(&mut self.store).await?;
        Ok(result)
    }

    async fn call_float32(&mut self, function: &str) -> Result<f32> {
        let func = self
            .instance
            .exports(&mut self.store)
            .root()
            .typed_func::<(), (f32,)>(function)?;
        let result = func.call_async(&mut self.store, ()).await?.0;
        func.post_return_async(&mut self.store).await?;
        Ok(result)
    }

    async fn call_float64(&mut self, function: &str) -> Result<f64> {
        let func = self
            .instance
            .exports(&mut self.store)
            .root()
            .typed_func::<(), (f64,)>(function)?;
        let result = func.call_async(&mut self.store, ()).await?.0;
        func.post_return_async(&mut self.store).await?;
        Ok(result)
    }

    async fn call_list_u8(&mut self, function: &str) -> Result<Vec<u8>> {
        let func = self
            .instance
            .exports(&mut self.store)
            .root()
            .typed_func::<(), (Vec<u8>,)>(function)?;
        let result = func.call_async(&mut self.store, ()).await?.0;
        func.post_return_async(&mut self.store).await?;
        Ok(result)
    }
}

pub fn generate_bindings(wit_path: &Path, world: Option<&str>, output_dir: &Path) -> Result<()> {
    let (resolve, world) = parse_wit(wit_path, world)?;
    let summary = Summary::try_new(&resolve, world)?;
    fs::create_dir_all(output_dir)?;
    summary.generate_code(output_dir)
}

#[allow(clippy::type_complexity)]
pub async fn componentize(
    wit_path: &Path,
    world: Option<&str>,
    python_path: &str,
    app_name: &str,
    output_path: &Path,
    add_to_linker: Option<&dyn Fn(&mut Linker<Ctx>) -> Result<()>>,
) -> Result<()> {
    let stdlib = tempfile::tempdir()?;

    Archive::new(Decoder::new(Cursor::new(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/python-lib.tar.zst"
    ))))?)
    .unpack(stdlib.path())?;

    let (resolve, world) = parse_wit(wit_path, world)?;
    let summary = Summary::try_new(&resolve, world)?;
    let symbols = summary.collect_symbols();

    let mut linker = wit_component::Linker::default()
        .validate(true)
        .library(
            "libcomponentize_py_runtime.so",
            &zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libcomponentize_py_runtime.so.zst"
            ))))?,
            false,
        )?
        .library(
            "libpython3.11.so",
            &zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libpython3.11.so.zst"
            ))))?,
            false,
        )?
        .library(
            "libc.so",
            &zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libc.so.zst"
            ))))?,
            false,
        )?
        .library(
            "libwasi-emulated.so",
            &zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libwasi-emulated.so.zst"
            ))))?,
            false,
        )?
        .library(
            "libc++.so",
            &zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libc++.so.zst"
            ))))?,
            false,
        )?
        .library(
            "libc++abi.so",
            &zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libc++abi.so.zst"
            ))))?,
            false,
        )?
        .library(
            "libcomponentize_py_bindings.so",
            &bindings::make_bindings(&resolve, world, &summary)?,
            false,
        )?
        .adapter(
            "wasi_snapshot_preview1",
            &zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/wasi_snapshot_preview1.wasm.zst"
            ))))?,
        )?;

    for (index, path) in python_path.split(NATIVE_PATH_DELIMITER).enumerate() {
        let mut libraries = Vec::new();
        find_native_extensions(Path::new(path), &mut libraries)?;
        for library in libraries {
            linker = linker.library(
                &format!(
                    "/{index}/{}",
                    library
                        .strip_prefix(path)
                        .unwrap()
                        .to_str()
                        .context("non-UTF-8 path")?
                ),
                &fs::read(&library)?,
                true,
            )?
        }
    }

    let component = linker.encode()?;

    let generated_code = tempfile::tempdir()?;
    let world_dir = generated_code
        .path()
        .join(resolve.worlds[world].name.to_snake_case());
    fs::create_dir_all(&world_dir)?;
    summary.generate_code(&world_dir)?;

    let python_path = format!(
        "{python_path}{NATIVE_PATH_DELIMITER}{}",
        generated_code
            .path()
            .to_str()
            .context("non-UTF-8 temporary directory name")?
    );

    let stdout = MemoryOutputPipe::new();
    let stderr = MemoryOutputPipe::new();

    let mut wasi = WasiCtxBuilder::new()
        .set_stdin(MemoryInputPipe::new(Bytes::new()))
        .set_stdout(stdout.clone())
        .set_stderr(stderr.clone())
        .push_env("PYTHONUNBUFFERED", "1")
        .push_env("COMPONENTIZE_PY_APP_NAME", app_name)
        .push_env("PYTHONHOME", "/python")
        .push_preopened_dir(
            Dir::from_std_file(File::open(stdlib.path())?),
            DirPerms::all(),
            FilePerms::all(),
            "python",
        );

    let mut count = 0;
    for (index, path) in python_path.split(NATIVE_PATH_DELIMITER).enumerate() {
        wasi = wasi.push_preopened_dir(
            Dir::from_std_file(File::open(path)?),
            DirPerms::all(),
            FilePerms::all(),
            &index.to_string(),
        );
        count += 1;
    }

    let python_path = (0..count)
        .map(|index| format!("/{index}"))
        .collect::<Vec<_>>()
        .join(":");

    let mut table = Table::new();
    let wasi = wasi
        .push_env("PYTHONPATH", format!("/python:{python_path}"))
        .build(&mut table)?;

    let mut config = Config::new();
    config.wasm_component_model(true);
    config.async_support(true);

    let engine = Engine::new(&config)?;

    let mut linker = Linker::new(&engine);
    let added_to_linker = if let Some(add_to_linker) = add_to_linker {
        add_to_linker(&mut linker)?;
        true
    } else {
        false
    };

    let mut store = Store::new(&engine, Ctx { wasi, table });

    let app_name = app_name.to_owned();
    let component = component_init::initialize(&component, move |instrumented| {
        async move {
            let component = &Component::new(&engine, instrumented)?;
            if !added_to_linker {
                add_wasi_and_stubs(&resolve, world, component, &mut linker)?;
            }

            let (init, instance) = Init::instantiate_async(&mut store, component, &linker).await?;

            init.exports()
                .call_init(&mut store, &app_name, &symbols)
                .await?
                .map_err(|e| anyhow!("{e}"))?;

            Ok(Box::new(MyInvoker { store, instance }) as Box<dyn Invoker>)
        }
        .boxed()
    })
    .await
    .with_context(move || {
        format!(
            "{}{}",
            String::from_utf8_lossy(&stdout.try_into_inner().unwrap()),
            String::from_utf8_lossy(&stderr.try_into_inner().unwrap())
        )
    })?;

    fs::write(output_path, component)?;

    Ok(())
}

fn parse_wit(path: &Path, world: Option<&str>) -> Result<(Resolve, WorldId)> {
    let mut resolve = Resolve::default();
    let pkg = if path.is_dir() {
        resolve.push_dir(path)?.0
    } else {
        let pkg = UnresolvedPackage::parse_file(path)?;
        resolve.push(pkg)?
    };
    let world = resolve.select_world(pkg, world)?;
    Ok((resolve, world))
}

fn add_wasi_and_stubs(
    resolve: &Resolve,
    world: WorldId,
    component: &Component,
    linker: &mut Linker<Ctx>,
) -> Result<()> {
    wasi_command::add_to_linker(linker)?;

    for (key, item) in &resolve.worlds[world].imports {
        let interface_name = match key {
            WorldKey::Name(name) => name.clone(),
            WorldKey::Interface(interface) => {
                let interface = &resolve.interfaces[*interface];
                format!(
                    "{}{}",
                    if let Some(package) = interface.package {
                        let package = &resolve.packages[package];
                        format!("{}:{}/", package.name.namespace, package.name.name)
                    } else {
                        String::new()
                    },
                    interface.name.as_deref().unwrap()
                )
            }
        };

        match item {
            WorldItem::Interface(interface) => {
                let interface = &resolve.interfaces[*interface];
                for function_name in interface.functions.keys() {
                    linker
                        .instance(&interface_name)?
                        .func_new(component, function_name, {
                            let interface_name = interface_name.clone();
                            let function_name = function_name.clone();
                            move |_, _, _| {
                                Err(anyhow!(
                                    "called trapping stub: {interface_name}#{function_name}"
                                ))
                            }
                        })?;
                }
            }
            WorldItem::Function(function) => {
                linker.root().func_new(component, &function.name, {
                    let function_name = function.name.clone();
                    move |_, _, _| Err(anyhow!("called trapping stub: {function_name}"))
                })?;
            }
            WorldItem::Type(_) => unreachable!(),
        }
    }

    Ok(())
}

fn find_native_extensions(path: &Path, libraries: &mut Vec<PathBuf>) -> Result<()> {
    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            find_native_extensions(&entry?.path(), libraries)?;
        }
    } else if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
        if name.ends_with(NATIVE_EXTENSION_SUFFIX) {
            libraries.push(path.to_owned());
        }
    }

    Ok(())
}

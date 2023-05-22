#![deny(warnings)]

use {
    anyhow::{Context, Error, Result},
    heck::ToSnakeCase,
    once_cell::sync::Lazy,
    std::{
        collections::{hash_map::Entry, HashMap},
        env,
        fs::{self, File},
        io::{self, Cursor, Read, Seek},
        path::Path,
        rc::Rc,
        str,
        sync::Mutex,
    },
    summary::Summary,
    tar::Archive,
    wasi_common::WasiCtx,
    wasmtime::Linker,
    wasmtime_wasi::{sync::Dir, WasiCtxBuilder, WasiFile},
    wit_parser::{Resolve, UnresolvedPackage, WorldId},
    wizer::Wizer,
    zstd::Decoder,
};

mod abi;
mod bindgen;
pub mod command;
mod componentize;
mod convert;
#[cfg(feature = "pyo3")]
mod python;
mod summary;
#[cfg(test)]
mod test;
mod util;

#[cfg(unix)]
const NATIVE_PATH_DELIMITER: char = ':';

#[cfg(windows)]
const NATIVE_PATH_DELIMITER: char = ';';

static WASI_TABLE: Lazy<Mutex<HashMap<u32, WasiCtx>>> = Lazy::new(|| Mutex::new(HashMap::new()));

struct WasiContext {
    key: u32,
}

impl WasiContext {
    fn new(ctx: WasiCtx) -> Self {
        let mut key = 0;
        loop {
            let mut table = WASI_TABLE.lock().unwrap();
            match table.entry(key) {
                Entry::Occupied(_) => {
                    key += 1;
                }
                Entry::Vacant(entry) => {
                    entry.insert(ctx);
                    break Self { key };
                }
            }
        }
    }
}

impl Drop for WasiContext {
    fn drop(&mut self) {
        WASI_TABLE.lock().unwrap().remove(&self.key);
    }
}

fn get_wasi(ctx: &mut Option<WasiCtx>, key: u32) -> &mut WasiCtx {
    if ctx.is_none() {
        *ctx = WASI_TABLE.lock().unwrap().remove(&key);
    }

    ctx.as_mut().unwrap()
}

fn open_dir(path: impl AsRef<Path>) -> Result<Dir> {
    Dir::open_ambient_dir(path, wasmtime_wasi::sync::ambient_authority()).map_err(Error::from)
}

fn file(file: File) -> Box<dyn WasiFile + 'static> {
    Box::new(wasmtime_wasi::file::File::from_cap_std(
        cap_std::fs::File::from_std(file),
    ))
}

pub fn generate_bindings(wit_path: &Path, world: Option<&str>, output_dir: &Path) -> Result<()> {
    let (resolve, world) = parse_wit(wit_path, world)?;
    let summary = Summary::try_new(&resolve, world)?;
    fs::create_dir_all(output_dir)?;
    summary.generate_code(output_dir)
}

pub fn componentize(
    wit_path: &Path,
    world: Option<&str>,
    python_path: &str,
    app_name: &str,
    stub_wasi: bool,
    output_path: &Path,
) -> Result<()> {
    let stdlib = tempfile::tempdir()?;

    Archive::new(Decoder::new(Cursor::new(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/python-lib.tar.zst"
    ))))?)
    .unpack(stdlib.path())?;

    let (resolve, world) = parse_wit(wit_path, world)?;
    let summary = Summary::try_new(&resolve, world)?;

    let symbols = tempfile::tempdir()?;
    bincode::serialize_into(
        &mut File::create(symbols.path().join("bin"))?,
        &summary.collect_symbols(),
    )?;

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

    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    let stdin = tempfile::tempfile()?;

    let mut wasi = WasiCtxBuilder::new()
        .stdin(file(stdin))
        .stdout(file(stdout.try_clone()?))
        .stderr(file(stdout.try_clone()?))
        .env("PYTHONUNBUFFERED", "1")?
        .env("COMPONENTIZE_PY_APP_NAME", app_name)?
        .env("PYTHONHOME", "/python")?
        .env("COMPONENTIZE_PY_SYMBOLS_PATH", "/symbols/bin")?
        .preopened_dir(open_dir(stdlib.path())?, "python")?
        .preopened_dir(open_dir(symbols.path())?, "symbols")?;

    let mut count = 0;
    for (index, path) in python_path.split(NATIVE_PATH_DELIMITER).enumerate() {
        wasi = wasi.preopened_dir(open_dir(path)?, &index.to_string())?;
        count += 1;
    }

    let python_path = (0..count)
        .map(|index| format!("/{index}"))
        .collect::<Vec<_>>()
        .join(":");

    let context = WasiContext::new(
        wasi.env("PYTHONPATH", &format!("/python:{python_path}"))?
            .build(),
    );
    let key = context.key;

    let module = Wizer::new()
        .wasm_bulk_memory(true)
        .make_linker(Some(Rc::new(move |engine| {
            let mut linker = Linker::new(engine);
            wasmtime_wasi::add_to_linker(&mut linker, move |ctx| get_wasi(ctx, key))?;
            Ok(linker)
        })))?
        .run(&zstd::decode_all(Cursor::new(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/runtime.wasm.zst"
        ))))?)
        .with_context(move || {
            let mut buffer = String::new();
            if stdout.rewind().is_ok() {
                _ = stdout.read_to_string(&mut buffer);
            }

            if stderr.rewind().is_ok() {
                _ = stderr.read_to_string(&mut buffer);
                _ = io::copy(&mut stderr, &mut io::stderr().lock());
            }

            buffer
        })?;

    let component = componentize::componentize(&module, &resolve, world, &summary, stub_wasi)?;

    fs::write(output_path, component)?;

    Ok(())
}

fn parse_wit(path: &Path, world: Option<&str>) -> Result<(Resolve, WorldId)> {
    let mut resolve = Resolve::default();
    let pkg = if path.is_dir() {
        resolve.push_dir(path)?.0
    } else {
        let pkg = UnresolvedPackage::parse_file(path)?;
        resolve.push(pkg, &Default::default())?
    };
    let world = resolve.select_world(pkg, world)?;
    Ok((resolve, world))
}

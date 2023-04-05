#![deny(warnings)]

use {
    anyhow::{bail, Context, Result},
    clap::Parser,
    std::{
        env, fs,
        io::{self, Cursor, Seek},
        path::{Path, PathBuf},
        process::Command,
        str,
    },
    tar::Archive,
    wizer::Wizer,
    zstd::Decoder,
};

#[cfg(unix)]
const NATIVE_PATH_DELIMITER: char = ':';

#[cfg(windows)]
const NATIVE_PATH_DELIMITER: char = ';';

// Assume Wasm32
// TODO: Wasm64 support
const WORD_SIZE: usize = 4;
const WORD_ALIGN: usize = 2; // as a power of two

/// A utility to convert Python apps into Wasm components
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Options {
    /// The name of a Python module containing the app to wrap
    app_name: String,

    /// File or directory containing WIT document(s)
    #[arg(short = 'd', long, default_value = "wit")]
    wit_path: PathBuf,

    /// Name of world to target (or default world if `None`)
    #[arg(short = 'w', long)]
    world: Option<String>,

    /// `PYTHONPATH` for specifying directory containing the app and optionally other directories containing
    /// dependencies.
    ///
    /// If `pipenv` is in `$PATH` and `pipenv --venv` produces a path containing a `site-packages` subdirectory,
    /// that directory will be appended to this value as a convenience for `pipenv` users.
    #[arg(short = 'p', long, default_value = ".")]
    python_path: String,

    /// Output file to write the resulting module to
    #[arg(short = 'o', long, default_value = "index.wasm")]
    output: PathBuf,
}

#[derive(Parser, Debug)]
struct PrivateOptions {
    app_name: String,
    wit_path: PathBuf,
    #[arg(long)]
    world: Option<String>,
    python_home: String,
    python_path: String,
    output: PathBuf,
    wit_path: PathBuf,
}

fn main() -> Result<()> {
    if env::var_os("COMPONENTIZE_PY_WIZEN").is_some() {
        let options = PrivateOptions::parse();

        env::remove_var("COMPONENTIZE_PY_WIZEN");

        env::set_var("PYTHONUNBUFFERED", "1");
        env::set_var("COMPONENTIZE_PY_APP_NAME", &options.app_name);

        let mut wizer = Wizer::new();

        wizer
            .allow_wasi(true)?
            .inherit_env(true)
            .inherit_stdio(true)
            .wasm_bulk_memory(true);

        let python_path = options
            .python_path
            .split(NATIVE_PATH_DELIMITER)
            .enumerate()
            .map(|(index, path)| {
                let index = index.to_string();
                wizer.map_dir(&index, path);
                format!("/{index}")
            })
            .collect::<Vec<_>>()
            .join(":");

        wizer.map_dir("python", &options.python_home);

        env::set_var("PYTHONPATH", format!("/python:{python_path}"));
        env::set_var("PYTHONHOME", "/python");

        let module = wizer.run(&zstd::decode_all(Cursor::new(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/runtime.wasm.zst"
        ))))?)?;

        let (resolve, world) = parse_wit(&options.wit_path, options.wit_world.as_deref())?;
        let component = componentize(&module, &resolve, world, &Summary::try_new(resolve, world)?)?;

        fs::write(&options.output, component)?;
    } else {
        let options = Options::parse();

        let stdlib = tempfile::tempdir()?;

        Archive::new(Decoder::new(Cursor::new(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/python-lib.tar.zst"
        ))))?)
        .unpack(stdlib.path())?;

        let generated_code = tempfile::tempdir()?;
        let (resolve, world) = parse_wit(&options.wit_path, options.wit_world.as_deref())?;
        let summary = Summary::try_new(resolve, world)?;
        summary.generate_code(generated_code.path())?;

        let mut python_path = format!(
            "{}{NATIVE_PATH_DELIMITER}{}",
            options.python_path,
            generated_code
                .path()
                .to_str()
                .context("non-UTF-8 temporary directory name")?
        );

        if let Some(site_packages) = find_site_packages()? {
            python_path = format!(
                "{python_path}{NATIVE_PATH_DELIMITER}{}",
                site_packages
                    .to_str()
                    .context("non-UTF-8 site-packages name")?
            )
        }

        // Spawn a subcommand to do the real work.  This gives us an opportunity to clear the environment so that
        // build-time environment variables don't end up in the Wasm module we're building.
        //
        // Note that we need to use temporary files for stdio instead of the default inheriting behavior since (as
        // of this writing) CPython interacts poorly with Wasmtime's WASI implementation if any of the stdio
        // descriptors point to non-files on Windows.  Specifically, the WASI implementation will trap when CPython
        // calls `fd_filestat_get` on non-files.

        let mut stdin = tempfile::tempfile()?;
        let mut stdout = tempfile::tempfile()?;
        let mut stderr = tempfile::tempfile()?;

        bincode::serialize_into(&mut stdin, &summary.collect_symbols())?;
        stdin.rewind()?;

        let mut cmd = Command::new(env::args().next().unwrap());
        cmd.env_clear()
            .env("COMPONENTIZE_PY_WIZEN", "1")
            .arg(&options.app_name)
            .arg(&options.wit_path)
            .arg(
                stdlib
                    .path()
                    .to_str()
                    .context("non-UTF-8 temporary directory name")?,
            )
            .arg(&python_path)
            .arg(&options.output)
            .stdin(stdin)
            .stdout(stdout.try_clone()?)
            .stderr(stderr.try_clone()?);

        if let Some(world) = &options.world {
            cmd.arg("--world").arg(world);
        }

        let status = cmd.status()?;

        if !status.success() {
            stdout.rewind()?;
            io::copy(&mut stdout, &mut io::stdout().lock())?;

            stderr.rewind()?;
            io::copy(&mut stderr, &mut io::stderr().lock())?;

            bail!("Couldn't create wasm from input");
        }

        println!("Component built successfully");
    }

    Ok(())
}

fn find_site_packages() -> Result<Option<PathBuf>> {
    Ok(match Command::new("pipenv").arg("--venv").output() {
        Ok(output) => {
            if output.status.success() {
                let dir = Path::new(str::from_utf8(&output.stdout)?.trim()).join("lib");

                if let Some(site_packages) = find_dir("site-packages", &dir)? {
                    Some(site_packages)
                } else {
                    eprintln!(
                        "warning: site-packages directory not found under {}",
                        dir.display()
                    );
                    None
                }
            } else {
                // `pipenv` is in `$PATH`, but this app does not appear to be using it
                None
            }
        }
        Err(_) => {
            // `pipenv` is not in `$PATH -- assume this app isn't using it
            None
        }
    })
}

fn find_dir(name: &str, path: &Path) -> Result<Option<PathBuf>> {
    if path.is_dir() {
        match path.file_name().and_then(|name| name.to_str()) {
            Some(this_name) if this_name == name => {
                return Ok(Some(path.canonicalize()?));
            }
            _ => {
                for entry in fs::read_dir(path)? {
                    if let Some(path) = find_dir(name, &entry?.path())? {
                        return Ok(Some(path));
                    }
                }
            }
        }
    }

    Ok(None)
}

macro_rules! declare_enum {
    ($name:ident { $( $variant ),* } $list:ident) => {
        #[derive(Debug, Copy, Clone, Hash, PartialEq, Eq)]
        enum $name {
            $( $variant ),*
        }

        static $list: &[$name] = $[$( $name::$variant ),*];
    }
}

declare_enum! {
    Link {
        Dispatch,
        Free,
        LowerI32,
        LowerI64,
        LowerF32,
        LowerF64,
        LowerString,
        GetField,
        GetListLength,
        GetListElement,
        Allocate,
        LiftI32,
        LiftI64,
        LiftF32,
        LiftF64,
        LiftString,
        Init,
        MakeList,
        ListAppend
    } LINK_LIST
}

struct Abi {
    size: usize,
    align: usize,
    flattened: Vec<ValType>,
}

fn record_abi(resolve: &Resolve, types: impl IntoIterator<Item = Type>) -> Abi {
    let mut size = 0;
    let mut align = 1;
    let flattened = Vec::new();
    for ty in types {
        let abi = abi(self.resolve, ty);
        align(&mut size, abi.align);
        size += abi.size;
        flattened.extend(abi.flattened);
    }

    Abi {
        size,
        align,
        flattened,
    }
}

fn abi(resolve: &Resolve, ty: Type) -> Abi {
    match ty {
        Type::Bool | Type::U8 | Type::S8 => Abi {
            size: 1,
            align: 1,
            flattened: vec![ValType::I32],
        },
        Type::U16 | Type::S16 => Abi {
            size: 2,
            align: 2,
            flattened: vec![ValType::I32],
        },
        Type::U32 | Type::S32 | Type::Char => Abi {
            size: 4,
            align: 4,
            flattened: vec![ValType::I32],
        },
        Type::U64 | Type::S64 => Abi {
            size: 8,
            align: 8,
            flattened: vec![ValType::I64],
        },
        Type::Float32 => Abi {
            size: 4,
            align: 4,
            flattened: vec![ValType::F32],
        },
        Type::Float64 => Abi {
            size: 8,
            align: 8,
            flattened: vec![ValType::F64],
        },
        Type::String => Abi {
            size: 8,
            align: 4,
            flattened: vec![ValType::I32, ValType::I32],
        },
        Type::Id(id) => match self.resolve.types[id].kind {
            TypeDefKind::Record(record) => {
                record_abi(resolve, record.fields.iter().map(|field| field.ty))
            }
            TypeDefKind::List(element_type) => Abi {
                size: 8,
                align: 4,
                flattened: vec![ValType::I32, ValType::I32],
            },
            _ => todo!(),
        },
    }
}

struct FunctionBindgen<'a> {
    resolve: &'a Resolve,
    stack_pointer: u32,
    link_map: &'a HashMap<Link, u32>,
    types: &'a IndexSet<TypeId>,
    params: &'a [(String, Type)],
    results: &'a Results,
    params_abi: Abi,
    results_abi: Abi,
    local_types: Vec<ValType>,
    local_stack: Vec<bool>,
    instructions: Vec<Ins>,
}

impl<'a> FunctionBindgen<'a> {
    fn new(
        resolve: &'a Resolve,
        stack_pointer: u32,
        link_map: &'a HashMap<Link, u32>,
        types: &'a IndexSet<TypeId>,
        params: &'a [(String, Type)],
        results: &'a Results,
    ) -> Self {
        Self {
            resolve,
            stack_pointer,
            link_map,
            types,
            params,
            results,
            params_abi: record_abi(resolve, params.types()),
            results_abi: record_abi(resolve, results.types()),
            local_types: Vec::new(),
            local_stack: Vec::new(),
            instructions: Vec::new(),
        }
    }

    fn compile_import(&mut self, index: usize) {
        // Arg 0: *const Python
        let context = 0;
        // Arg 1: *const &PyAny
        let input = 1;
        // Arg 2: *mut &PyAny
        let output = 2;

        let locals = if self.params_abi.flattened.len() <= MAX_FLAT_PARAMS {
            let locals = self
                .params_abi
                .flattened
                .iter()
                .map(|ty| {
                    let local = self.push_local(ty);
                    self.push(Ins::LocalSet(local));
                    local
                })
                .collect::<Vec<_>>();

            let mut load_offset = 0;
            for ty in self.params.types() {
                let value = self.push_local(ValType::I32);

                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(input));
                self.push(Ins::I32Load(mem_arg(load_offset, WORD_ALIGN)));
                self.push(Ins::LocalSet(value));

                self.lower(ty, context, value);

                for local in locals[lift_index..][..flat_count] {
                    self.push(Ins::LocalTee(local));
                }

                load_offset += WORD_SIZE;

                self.pop_local(value, ValType::I32);
            }

            Some(locals)
        } else {
            self.push_stack(self.params_abi.size);

            let mut store_offset = 0;
            for ty in self.params.types() {
                let value = self.push_local(ValType::I32);
                let destination = self.push_local(ValType::I32);

                let abi = abi(self.resolve, ty);
                align(&mut store_offset, abi.align);

                self.get_stack();
                self.push(Ins::I32Const(store_offset));
                self.push(Ins::I32Add);
                self.push(Ins::LocalSet(destination));

                self.push(Ins::LocalGet(input));
                self.push(Ins::I32Load(mem_arg(load_offset, WORD_ALIGN)));
                self.push(Ins::LocalSet(value));

                self.store(ty, context, value, destination);

                store_offset += abi.size;

                self.pop_local(destination, ValType::I32);
                self.pop_local(value, ValType::I32);
            }

            self.get_stack();

            None
        };

        if self.results_abi.flattened.len() > MAX_FLAT_RESULTS {
            self.push_stack(self.results_abi.size);

            self.get_stack();
        }

        self.push(Ins::Call(index));

        if self.results_abi.flattened.len() <= MAX_FLAT_RESULTS {
            let locals = self
                .results_abi
                .flattened
                .iter()
                .map(|ty| {
                    let local = self.push_local(ty);
                    self.push(Ins::LocalSet(local));
                    local
                })
                .collect::<Vec<_>>();

            self.lift_record(self.results.types(), context, &locals, output);

            for (local, ty) in locals.iter().zip(&self.results_abi.flattened).rev() {
                self.pop_local(local, ty);
            }
        } else {
            let source = self.push_local(ValType::I32);

            self.get_stack();
            self.push(Ins::LocalSet(source));

            self.load_record(self.results.types(), context, source, output);

            self.pop_local(source, ValType::I32);
            self.pop_stack(self.results_abi.size);
        }

        if let Some(locals) = locals {
            self.free_lowered_record(self.params.types(), &locals);

            for (local, ty) in locals.iter().zip(&self.params_abi.flattened).rev() {
                self.pop_local(local, ty);
            }
        } else {
            let value = self.push_local(ValType::I32);

            self.get_stack();
            self.push(Ins::LocalSet(value));

            self.free_stored_record(self.params.types(), value);

            self.pop_local(value, ValType::I32);
            self.pop_stack(self.params_abi.size);
        }
    }

    fn compile_export(&mut self, index: u32, lift: u32, lower: u32) {
        self.push(Ins::I32Const(index));
        self.push(Ins::I32Const(lift));
        self.push(Ins::I32Const(lower));
        self.push(Ins::I32Const(self.params.types().count()));

        let param_flat_count = if self.params_abi.flattened.len() <= MAX_FLAT_PARAMS {
            self.push_stack(self.params_abi.size);

            let destination = self.push_local(ValType::I32);
            self.get_stack();
            self.push(Ins::LocalSet(destination));

            self.store_copy_record(
                self.params.types(),
                &(0..self.params_abi.flattened.len()).collect::<Vec<_>>(),
                destination,
            );

            self.pop_local(destination, ValType::I32);

            self.get_stack();

            self.params_abi.flattened.len()
        } else {
            self.push(Ins::LocalGet(0));

            1
        };

        if self.results_abi.flattened.len() <= MAX_FLAT_RESULTS {
            self.push_stack(self.results_abi.size);

            self.get_stack();
        } else {
            self.push(Ins::LocalGet(param_flat_count));
        }

        self.link_call(Link::Dispatch);

        if self.results_abi.flattened.len() <= MAX_FLAT_RESULTS {
            let source = self.push_local(ValType::I32);
            self.get_stack();
            self.push(Ins::LocalSet(source));

            self.load_copy_record(self.results.types(), source);

            self.pop_local(source, ValType::I32);

            self.pop_stack(self.results_abi.size);
        }

        if self.params_abi.flattened.len() <= MAX_FLAT_PARAMS {
            self.pop_stack(self.params_abi.size);
        }
    }

    fn compile_export_lift(&mut self) {
        // Arg 0: *const Python
        let context = 0;
        // Arg 1: *const MyParams
        let source = 1;
        // Arg 2: *mut &PyAny
        let destination = 2;

        self.load_record(self.params.types(), context, source, destination);

        self.build()
    }

    fn compile_export_lower(&mut self) {
        // Arg 0: *const Python
        let context = 0;
        // Arg 1: *const &PyAny
        let source = 1;
        // Arg 2: *mut MyResults
        let destination = 2;

        self.store_record(self.results.types(), context, source, destination);

        self.build()
    }

    fn compile_export_post_return(&mut self) {
        if self.results_abi.flattened.len() > MAX_FLAT_RESULTS {
            // Arg 0: *mut MyResults
            let value = 0;

            let mut gen = FunctionBuilder::new(self);

            self.free_stored_record(self.results.types(), value);

            self.push(Ins::LocalGet(value));
            self.push(Ins::I32Const(self.results_abi.size));
            self.push(Ins::I32Const(self.results_abi.align));
            self.link_call(Link::Free);

            Some(self.build())
        } else {
            // As of this writing, no type involving heap allocation can fit into `MAX_FLAT_RESULTS`, so nothing to
            // do.  We'll need to revisit this if `MAX_FLAT_RESULTS` changes or if new types are added.
            None
        }
    }

    fn push_stack(&mut self, size: usize) {
        self.push(Ins::GlobalGet(self.stack_pointer));
        self.push(Ins::I32Const(align(size, WORD_SIZE)));
        self.push(Ins::I32Sub);
        self.push(Ins::GlobalSet(self.stack_pointer));
    }

    fn pop_stack(&mut self, size: usize) {
        self.push(Ins::GlobalGet(self.stack_pointer));
        self.push(Ins::I32Const(align(size, WORD_SIZE)));
        self.push(Ins::I32Add);
        self.push(Ins::GlobalSet(self.stack_pointer));
    }

    fn push(&mut self, instruction: Ins) {
        self.instructions.push(instruction)
    }

    fn link_call(&mut self, link: Link) {
        self.push(Ins::Call(self.link_map.get(link).unwrap()));
    }

    fn get_stack(&mut self) {
        self.push(Ins::GlobalGet(self.stack_pointer));
    }

    fn push_local(&mut self, ty: ValType) -> u32 {
        while self.local_types.len() > self.local_stack.len()
            && self.local_types[self.local_stack.len()] != ty
        {
            self.local_stack.push(false);
        }

        self.local_stack.push(true);
        if self.local_types.len() < self.local_stack.len() {
            self.local_types.push(ty);
        }

        self.params_abi.flattened.len() + self.local_stack.len() - 1
    }

    fn pop_local(&mut self, index: u32, ty: ValType) {
        assert!(index == self.params_abi.flattened.len() + self.local_stack.len() - 1);
        assert!(ty == self.local_types.len() - 1);

        self.local_stack.pop();
        while let Some(false) = self.local_stack.last() {
            self.local_stack.pop();
        }
    }

    fn lower(&mut self, ty: Type, context: u32, value: u32) {
        match ty {
            Type::Bool
            | Type::U8
            | Type::U16
            | Type::U32
            | Type::S8
            | Type::S16
            | Type::S32
            | Type::Char => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value));
                self.link_call(Link::LowerI32);
            }
            Type::U64 | Type::S64 => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value));
                self.link_call(Link::LowerI64);
            }
            Type::Float32 => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value));
                self.link_call(Link::LowerF32);
            }
            Type::Float64 => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value));
                self.link_call(Link::LowerF64);
            }
            Type::String => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value));
                self.push_stack(WORD_SIZE * 2);
                self.stack();
                self.link_call(Link::LowerString);
                self.stack();
                self.push(Ins::I32Load(mem_arg(0, WORD_ALIGN)));
                self.stack();
                self.push(Ins::I32Load(mem_arg(WORD_SIZE, WORD_ALIGN)));
                self.pop_stack(WORD_SIZE * 2);
            }
            Type::Id(id) => match self.resolve.types[id].kind {
                TypeDefKind::Record(record) => {
                    let type_index = self.types.get_index_of(id).unwrap();
                    for (field_index, field) in record.fields.iter().enumerate() {
                        let field_value = self.push_local(ValType::I32);

                        self.push(Ins::LocalGet(context));
                        self.push(Ins::LocalGet(value));
                        self.push(Ins::I32Const(type_index));
                        self.push(Ins::I32Const(field_index));
                        self.link_call(Link::GetField);
                        self.push(Ins::LocalSet(field_value));

                        self.lower(field.ty, context, field_value);

                        self.pop_local(field_value, ValType::I32);
                    }
                }
                TypeDefKind::List(ty) => {
                    // TODO: optimize `list<u8>` (and others if appropriate)

                    let abi = abi(self.resolve, ty);
                    let length = self.push_local(ValType::I32);
                    let index = self.push_local(ValType::I32);
                    let destination = self.push_local(ValType::I32);
                    let element_value = self.push_local(ValType::I32);
                    let element_destination = self.push_local(ValType::I32);

                    self.push(Ins::LocalGet(context));
                    self.push(Ins::LocalGet(value));
                    self.link_call(Link::GetListLength);
                    self.push(Ins::LocalSet(length));

                    self.push(Ins::I32Const(0));
                    self.push(Ins::LocalSet(index));

                    self.push(Ins::LocalGet(context));
                    self.push(Ins::LocalGet(length));
                    self.push(Ins::I32Const(abi.size));
                    self.push(Ins::I32Mul);
                    self.push(Ins::I32Const(abi.align));
                    self.link_call(Link::Allocate);
                    self.push(Ins::LocalSet(destination));

                    let loop_ = self.push_block();
                    self.push(Ins::Loop(BlockType::Empty));

                    self.push(Ins::LocalGet(index));
                    self.push(Ins::LocalGet(length));
                    self.push(Ins::I32Ne);

                    self.push(Ins::If(BlockType::Empty));

                    self.push(Ins::LocalGet(context));
                    self.push(Ins::LocalGet(value));
                    self.push(Ins::LocalGet(index));
                    self.link_call(Link::GetListElement);
                    self.push(Ins::LocalSet(element_value));

                    self.push(Ins::LocalGet(destination));
                    self.push(Ins::LocalGet(index));
                    self.push(Ins::I32Const(abi.size));
                    self.push(Ins::I32Mul);
                    self.push(Ins::I32Add);
                    self.push(Ins::LocalSet(element_destination));

                    self.store(ty, context, element_value, element_destination);

                    self.push(Ins::Br(loop_));

                    self.push(Ins::End);

                    self.push(Ins::End);
                    self.pop_block(loop_);

                    self.push(Ins::LocalGet(destination));
                    self.push(Ins::LocalGet(length));

                    self.pop_local(element_destination, ValType::I32);
                    self.pop_local(element_value, ValType::I32);
                    self.pop_local(destination, ValType::I32);
                    self.pop_local(index, ValType::I32);
                    self.pop_local(length, ValType::I32);
                }
                _ => todo!(),
            },
        }
    }

    fn store(&mut self, ty: Type, context: u32, value: u32, destination: u32) {
        match ty {
            Type::Bool | Type::U8 | Type::S8 => {
                self.lower(ty, context, value);
                self.push(Ins::LocalGet(destination));
                self.push(Ins::I32Store8(mem_arg(0, 0)));
            }
            Type::U16 | Type::S16 => {
                self.lower(ty, context, value);
                self.push(Ins::LocalGet(destination));
                self.push(Ins::I32Store16(mem_arg(0, 1)));
            }
            Type::U32 | Type::S32 | Type::Char => {
                self.lower(ty, context, value);
                self.push(Ins::LocalGet(destination));
                self.push(Ins::I32Store(mem_arg(0, 2)));
            }
            Type::U64 | Type::S64 => {
                self.lower(ty, context, value);
                self.push(Ins::LocalGet(destination));
                self.push(Ins::I64Store(mem_arg(0, 3)));
            }
            Type::Float32 => {
                self.lower(ty, context, value);
                self.push(Ins::LocalGet(destination));
                self.push(Ins::F32Store(mem_arg(0, 2)));
            }
            Type::Float64 => {
                self.lower(ty, context, value);
                self.push(Ins::LocalGet(destination));
                self.push(Ins::F64Store(mem_arg(0, 3)));
            }
            Type::String => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value));
                self.push(Ins::LocalGet(destination));
                self.link_call(Link::LowerString);
            }
            Type::Id(id) => match self.resolve.types[id].kind {
                TypeDefKind::Record(record) => {
                    let type_index = self.types.get_index_of(id).unwrap();
                    let mut store_offset = 0;
                    for (field_index, field) in record.fields.iter().enumerate() {
                        let abi = abi(self.resolve, ty);
                        align(&mut store_offset, abi.align);

                        let field_value = self.push_local(ValType::I32);
                        let field_destination = self.push_local(ValType::I32);

                        self.push(Ins::LocalGet(context));
                        self.push(Ins::LocalGet(value));
                        self.push(Ins::I32Const(type_index));
                        self.push(Ins::I32Const(field_index));
                        self.link_call(Link::GetField);
                        self.push(Ins::LocalSet(field_value));

                        self.push(Ins::LocalGet(destination));
                        self.push(Ins::I32Const(store_offset));
                        self.push(Ins::I32Add);
                        self.push(Ins::LocalSet(field_destination));

                        self.store(field.ty, context, field_value, field_destination);

                        store_offset += abi.size;

                        self.pop_local(field_destination, ValType::I32);
                        self.pop_local(field_value, ValType::I32);
                    }
                }
                TypeDefKind::List(element_type) => {
                    self.lower(ty, context, value);
                    self.push(Ins::LocalGet(destination));
                    self.push(Ins::I32Store(mem_arg(WORD_SIZE, WORD_ALIGN)));
                    self.push(Ins::LocalGet(destination));
                    self.push(Ins::I32Store(mem_arg(0, WORD_ALIGN)));
                }
                _ => todo!(),
            },
        }
    }

    fn store_copy(&mut self, ty: Type, source: &[u32], destination: u32) {
        match ty {
            Type::Bool | Type::U8 | Type::S8 => {
                self.push(Ins::LocalGet(source[0]));
                self.push(Ins::I32Store8(mem_arg(0, 0)));
            }
            Type::U16 | Type::S16 => {
                self.push(Ins::LocalGet(source[0]));
                self.push(Ins::I32Store16(mem_arg(0, 1)));
            }
            Type::U32 | Type::S32 | Type::Char => {
                self.push(Ins::LocalGet(source[0]));
                self.push(Ins::I32Store(mem_arg(0, 2)));
            }
            Type::U64 | Type::S64 => {
                self.push(Ins::LocalGet(source[0]));
                self.push(Ins::I64Store(mem_arg(0, 3)));
            }
            Type::Float32 => {
                self.push(Ins::LocalGet(source[0]));
                self.push(Ins::F32Store(mem_arg(0, 2)));
            }
            Type::Float64 => {
                self.push(Ins::LocalGet(source[0]));
                self.push(Ins::F64Store(mem_arg(0, 3)));
            }
            Type::String => {
                self.push(Ins::LocalGet(source[0]));
                self.push(Ins::LocalGet(destination));
                self.push(Ins::I32Store(mem_arg(0, WORD_ALIGN)));
                self.push(Ins::LocalGet(source[1]));
                self.push(Ins::LocalGet(destination));
                self.push(Ins::I32Store(mem_arg(WORD_SIZE, WORD_ALIGN)));
            }
            Type::Id(id) => match self.resolve.types[id].kind {
                TypeDefKind::Record(record) => {
                    self.store_copy_record(
                        record.fields.iter().map(|field| field.ty),
                        source,
                        destination,
                    );
                }
                TypeDefKind::List(element_type) => {
                    self.push(Ins::LocalGet(source[0]));
                    self.push(Ins::LocalGet(destination));
                    self.push(Ins::I32Store(mem_arg(0, WORD_ALIGN)));
                    self.push(Ins::LocalGet(source[1]));
                    self.push(Ins::LocalGet(destination));
                    self.push(Ins::I32Store(mem_arg(WORD_SIZE, WORD_ALIGN)));
                }
                _ => todo!(),
            },
        }
    }

    fn store_copy_record(
        &mut self,
        types: impl IntoIterator<Item = Type>,
        source: &[u32],
        destination: u32,
    ) {
        let local_index = 0;
        let mut store_offset = 0;
        for field in &record.fields {
            let abi = abi(self.resolve, ty);
            align(&mut store_offset, abi.align);

            let field_destination = self.push_local(ValType::I32);

            self.push(Ins::LocalGet(destination));
            self.push(Ins::I32Const(store_offset));
            self.push(Ins::I32Add);
            self.push(Ins::LocalSet(field_destination));

            self.store_copy(
                field.ty,
                source[local_index..][..abi.flat_count],
                field_destination,
            );

            local_index += abi.flat_count;
            store_offset += abi.size;

            self.pop_local(field_destination, ValType::I32);
        }
    }

    fn lift(&mut self, ty: Type, context: u32, value: &[u32]) {
        match ty {
            Type::Bool
            | Type::U8
            | Type::U16
            | Type::U32
            | Type::S8
            | Type::S16
            | Type::S32
            | Type::Char => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value[0]));
                self.link_call(Link::LiftI32);
            }
            Type::U64 | Type::S64 => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value[0]));
                self.link_call(Link::LiftI64);
            }
            Type::Float32 => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value[0]));
                self.link_call(Link::LiftF32);
            }
            Type::Float64 => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value[0]));
                self.link_call(Link::LiftF64);
            }
            Type::String => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value[0]));
                self.push(Ins::LocalGet(value[1]));
                self.link_call(Link::LiftString);
            }
            Type::Id(id) => match self.resolve.types[id].kind {
                TypeDefKind::Record(record) => {
                    self.push_stack(record.fields.len() * WORD_SIZE);
                    let source = self.push_local(ValType::I32);

                    self.get_stack();
                    self.push(Ins::LocalSet(source));

                    self.lift_record(record.fields.iter().map(|field| field.ty), context, source);

                    self.push(Ins::LocalGet(context));
                    self.push(Ins::I32Const(self.types.get_index_of(id).unwrap()));
                    self.get_stack();
                    self.push(Ins::I32Const(record.fields.len()));
                    self.link_call(Link::Init);

                    self.pop_local(source, ValType::I32);
                    self.pop_stack(record.fields.len() * WORD_SIZE);
                }
                TypeDefKind::List(ty) => {
                    // TODO: optimize using bulk memory operation when list element is primitive

                    let source = value[0];
                    let length = value[1];

                    let abi = abi(self.resolve, ty);

                    let index = self.push_local(ValType::I32);
                    let element_source = self.push_local(ValType::I32);

                    self.push(Ins::LocalGet(context));
                    self.link_call(Link::MakeList);
                    self.push(Ins::LocalSet(destination));

                    self.push(Ins::I32Const(0));
                    self.push(Ins::LocalSet(index));

                    let loop_ = self.push_block();
                    self.push(Ins::Loop(BlockType::Empty));

                    self.push(Ins::LocalGet(index));
                    self.push(Ins::LocalGet(length));
                    self.push(Ins::I32Ne);

                    self.push(Ins::If(BlockType::Empty));

                    self.push(Ins::LocalGet(source));
                    self.push(Ins::LocalGet(index));
                    self.push(Ins::I32Const(abi.size));
                    self.push(Ins::I32Mul);
                    self.push(Ins::I32Add);
                    self.push(Ins::LocalSet(element_source));

                    self.push(Ins::LocalGet(context));
                    self.push(Ins::LocalGet(destination));

                    self.load(ty, context, element_source);

                    self.link_call(Link::ListAppend);

                    self.push(Ins::Br(loop_));

                    self.push(Ins::End);

                    self.push(Ins::End);
                    self.pop_block(loop_);

                    self.push(Ins::LocalGet(destination));

                    self.pop_local(element_source, ValType::I32);
                    self.pop_local(index, ValType::I32);
                }
                _ => todo!(),
            },
        }
    }

    fn lift_record(
        &mut self,
        types: impl IntoIterator<Item = Type>,
        context: u32,
        source: &[u32],
        destination: u32,
    ) {
        let mut lift_index = 0;
        let mut store_offset = 0;
        for field in &record.fields {
            let flat_count = abi(self.resolve, ty).flat_count;

            self.lift(field.ty, context, &source[lift_index..][..flat_count]);

            self.push(Ins::LocalGet(destination));
            self.push(Ins::I32Store(mem_arg(store_offset, WORD_ALIGN)));

            lift_index += flat_count;
            store_offset += WORD_SIZE;
        }
    }

    fn load(&mut self, ty: Type, context: u32, source: u32) {
        match ty {
            Type::Bool | Type::U8 | Type::S8 => {
                let value = self.push_local(ValType::I32);
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load8(mem_arg(0, 0)));
                self.push(Ins::LocalSet(value));
                self.lift(ty, context, &[value]);
                self.pop_local(value, ValType::I32);
            }
            Type::U16 | Type::S16 => {
                let value = self.push_local(ValType::I32);
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load16(mem_arg(0, 1)));
                self.push(Ins::LocalSet(value));
                self.lift(ty, context, &[value]);
                self.pop_local(value, ValType::I32);
            }
            Type::U32 | Type::S32 | Type::Char => {
                let value = self.push_local(ValType::I32);
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load(mem_arg(0, 2)));
                self.push(Ins::LocalSet(value));
                self.lift(ty, context, &[value]);
                self.pop_local(value, ValType::I32);
            }
            Type::U64 | Type::S64 => {
                let value = self.push_local(ValType::I64);
                self.push(Ins::LocalGet(source));
                self.push(Ins::I64Load(mem_arg(0, 3)));
                self.push(Ins::LocalSet(value));
                self.lift(ty, context, &[value]);
                self.pop_local(value, ValType::I64);
            }
            Type::Float32 => {
                let value = self.push_local(ValType::F32);
                self.push(Ins::LocalGet(source));
                self.push(Ins::F32Load(mem_arg(0, 2)));
                self.push(Ins::LocalSet(value));
                self.lift(ty, context, &[value]);
                self.pop_local(value, ValType::F32);
            }
            Type::Float64 => {
                let value = self.push_local(ValType::F64);
                self.push(Ins::LocalGet(source));
                self.push(Ins::F64Load(mem_arg(0, 3)));
                self.push(Ins::LocalSet(value));
                self.lift(ty, context, &[value]);
                self.pop_local(value, ValType::F64);
            }
            Type::String => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load(mem_arg(0, WORD_ALIGN)));
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load(mem_arg(WORD_SIZE, WORD_ALIGN)));
                self.link_call(Link::LiftString);
            }
            Type::Id(id) => match self.resolve.types[id].kind {
                TypeDefKind::Record(record) => {
                    self.push_stack(record.fields.len() * WORD_SIZE);
                    let destination = self.push_local(ValType::I32);

                    self.get_stack();
                    self.push(Ins::LocalSet(destination));

                    self.load_record(
                        record.fields.iter().map(|field| field.ty),
                        context,
                        source,
                        destination,
                    );

                    self.push(Ins::I32Const(self.types.get_index_of(id).unwrap()));
                    self.get_stack();
                    self.push(Ins::I32Const(record.fields.len()));
                    self.link_call(Link::Init);

                    self.pop_local(destination, ValType::I32);
                    self.pop_stack(record.fields.len() * WORD_SIZE);
                }
                TypeDefKind::List(_) => {
                    let body = self.push_local(ValType::I32);
                    let length = self.push_local(ValType::I32);

                    self.push(Ins::LocalGet(source));
                    self.push(Ins::I32Load(mem_arg(0, WORD_ALIGN)));
                    self.push(Ins::LocalSet(body));

                    self.push(Ins::LocalGet(source));
                    self.push(Ins::I32Load(mem_arg(WORD_SIZE, WORD_ALIGN)));
                    self.push(Ins::LocalSet(length));

                    self.lift(ty, context, &[body, length]);

                    self.pop_local(length, ValType::I32);
                    self.pop_local(body, ValType::I32);
                }
                _ => todo!(),
            },
        }
    }

    fn load_record(
        &mut self,
        types: impl IntoIterator<Item = Type>,
        context: u32,
        source: u32,
        destination: u32,
    ) {
        let mut load_offset = 0;
        let mut store_offset = 0;
        for ty in types {
            let field_source = self.push_local(ValType::I32);

            let abi = abi(self.resolve, ty);
            align(&mut load_offset, abi.align);

            self.push(Ins::LocalGet(source));
            self.push(Ins::I32Const(load_offset));
            self.push(Ins::I32Add);
            self.load(Ins::LocalSet(field_source));

            self.load(ty, context, field_source);

            self.push(Inst::LocalGet(destination));
            self.push(Ins::I32Store(mem_arg(store_offset, WORD_ALIGN)));

            load_offset += abi.size;
            store_offset += WORD_SIZE;

            self.pop_local(field_source, ValType::I32);
        }
    }

    fn load_copy(&mut self, ty: Type, source: u32) {
        match ty {
            Type::Bool | Type::U8 | Type::S8 => {
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load8(mem_arg(0, 0)));
            }
            Type::U16 | Type::S16 => {
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load16(mem_arg(0, 1)));
            }
            Type::U32 | Type::S32 | Type::Char => {
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load(mem_arg(0, 2)));
            }
            Type::U64 | Type::S64 => {
                self.push(Ins::LocalGet(source));
                self.push(Ins::I64Load(mem_arg(0, 3)));
            }
            Type::Float32 => {
                self.push(Ins::LocalGet(source));
                self.push(Ins::F32Load(mem_arg(0, 2)));
            }
            Type::Float64 => {
                self.push(Ins::LocalGet(source));
                self.push(Ins::F64Load(mem_arg(0, 3)));
            }
            Type::String => {
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load(mem_arg(0, WORD_ALIGN)));
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load(mem_arg(WORD_SIZE, WORD_ALIGN)));
            }
            Type::Id(id) => match self.resolve.types[id].kind {
                TypeDefKind::Record(record) => {
                    self.load_copy_record(result.fields.iter().map(|field| field.ty), source);
                }
                TypeDefKind::List(_) => {
                    self.push(Ins::LocalGet(source));
                    self.push(Ins::I32Load(mem_arg(0, WORD_ALIGN)));
                    self.push(Ins::LocalGet(source));
                    self.push(Ins::I32Load(mem_arg(WORD_SIZE, WORD_ALIGN)));
                }
                _ => todo!(),
            },
        }
    }

    fn load_copy_record(&mut self, types: impl IntoIterator<Item = Type>, source: u32) {
        let mut load_offset = 0;
        for ty in types {
            let field_source = self.push_local(ValType::I32);

            let abi = abi(self.resolve, ty);
            align(&mut load_offset, abi.align);

            self.push(Ins::LocalGet(source));
            self.push(Ins::I32Const(load_offset));
            self.push(Ins::I32Add);
            self.load(Ins::LocalSet(field_source));

            self.load_copy(ty, field_source);

            load_offset += abi.size;

            self.pop_local(field_source, ValType::I32);
        }
    }

    fn free_lowered(&mut self, ty: Type, value: &[u32]) {
        match ty {
            Type::Bool
            | Type::U8
            | Type::U16
            | Type::U32
            | Type::S8
            | Type::S16
            | Type::S32
            | Type::Char
            | Type::U64
            | Type::S64
            | Type::Float32
            | Type::Float64 => {}

            Type::String => {
                self.push(Ins::LocalGet(value[0]));
                self.push(Ins::LocalGet(value[1]));
                self.push(Ins::I32Const(1));
                self.link_call(Link::Free);
            }

            Type::Id(id) => match self.resolve.types[id].kind {
                TypeDefKind::Record(record) => {
                    free_lowered_record(record.fields.iter().map(|field| field.ty), value);
                }
                TypeDefKind::List(ty) => {
                    // TODO: optimize (i.e. no loop) when list element is primitive

                    let pointer = value[0];
                    let length = value[1];

                    let abi = abi(self.resolve, ty);

                    let index = self.push_local(ValType::I32);
                    let element_pointer = self.push_local(ValType::I32);

                    self.push(Ins::I32Const(0));
                    self.push(Ins::LocalSet(index));

                    let loop_ = self.push_block();
                    self.push(Ins::Loop(BlockType::Empty));

                    self.push(Ins::LocalGet(index));
                    self.push(Ins::LocalGet(length));
                    self.push(Ins::I32Ne);

                    self.push(Ins::If(BlockType::Empty));

                    self.push(Ins::LocalGet(pointer));
                    self.push(Ins::LocalGet(index));
                    self.push(Ins::I32Const(abi.size));
                    self.push(Ins::I32Mul);
                    self.push(Ins::I32Add);
                    self.push(Ins::LocalSet(element_pointer));

                    self.free_stored(ty, element_pointer);

                    self.push(Ins::Br(loop_));

                    self.push(Ins::End);

                    self.push(Ins::End);
                    self.pop_block(loop_);

                    self.push(Ins::LocalGet(pointer));
                    self.push(Ins::LocalGet(index));
                    self.push(Ins::I32Const(abi.size));
                    self.push(Ins::I32Mul);
                    self.push(Ins::I32Const(abi.align));
                    self.link_call(Link::Free);

                    self.pop_local(element_pointer, ValType::I32);
                    self.pop_local(index, ValType::I32);
                }
                _ => todo!(),
            },
        }
    }

    fn free_lowered_record(&mut self, types: impl IntoIterator<Item = Type>, value: &[u32]) {
        let mut lift_index = 0;
        for field in &record.fields {
            let flat_count = abi(self.resolve, ty).flat_count;

            self.free_lowered(field.ty, context, &source[lift_index..][..flat_count]);

            lift_index += flat_count;
        }
    }

    fn free_stored(&mut self, ty: Type, value: u32) {
        match ty {
            Type::Bool
            | Type::U8
            | Type::U16
            | Type::U32
            | Type::S8
            | Type::S16
            | Type::S32
            | Type::Char
            | Type::U64
            | Type::S64
            | Type::Float32
            | Type::Float64 => {}

            Type::String => {
                self.push(Ins::LocalGet(value));
                self.push(Ins::I32Load(mem_arg(0, WORD_ALIGN)));
                self.push(Ins::LocalGet(value));
                self.push(Ins::I32Load(mem_arg(WORD_SIZE, WORD_ALIGN)));
                self.push(Ins::I32Const(1));
                self.link_call(Link::Free);
            }

            Type::Id(id) => match self.resolve.types[id].kind {
                TypeDefKind::Record(record) => {
                    free_stored_record(record.fields.iter().map(|field| field.ty), value);
                }
                TypeDefKind::List(ty) => {
                    let body = self.push_local(ValType::I32);
                    let length = self.push_local(ValType::I32);

                    self.push(Ins::LocalGet(value));
                    self.push(Ins::I32Load(mem_arg(0, WORD_ALIGN)));
                    self.push(Ins::LocalSet(body));

                    self.push(Ins::LocalGet(value));
                    self.push(Ins::I32Load(mem_arg(WORD_SIZE, WORD_ALIGN)));
                    self.push(Ins::LocalSet(length));

                    self.free_stored(ty, context, &[body, length]);

                    self.pop_local(length, ValType::I32);
                    self.pop_local(body, ValType::I32);
                }
                _ => todo!(),
            },
        }
    }

    fn free_stored_record(&mut self, types: impl IntoIterator<Item = Type>, value: u32) {
        let mut load_offset = 0;
        let mut store_offset = 0;
        for ty in types {
            let field_value = self.push_local(ValType::I32);

            let abi = abi(self.resolve, ty);
            align(&mut load_offset, abi.align);

            self.push(Ins::LocalGet(value));
            self.push(Ins::I32Const(load_offset));
            self.push(Ins::I32Add);
            self.load(Ins::LocalSet(field_value));

            self.free_stored(ty, field_source);

            load_offset += abi.size;

            self.pop_local(field_value, ValType::I32);
        }
    }
}

fn mem_arg(offset: u64, align: u32) -> MemArg {
    MemArg {
        offset,
        align,
        memory_index: 0,
    }
}

enum FunctionKind {
    Import,
    Export,
    ExportLift,
    ExportLower,
    ExportPostReturn,
}

struct MyFunction<'a> {
    kind: FunctionKind,
    interface: Option<&'a str>,
    name: &'a str,
    params: &'a [(String, Type)],
    results: &'a Results,
}

impl<'a> MyFunction<'a> {
    fn internal_name(&self) -> String {
        if let Some(interface) = self.interface {
            format!("{}#{}", interface, self.name);
        } else {
            self.name.to_owned()
        }
    }

    fn canonical_core_type(&self, resolve: &Resolve) -> (Vec<ValType>, Vec<ValType>) {
        (
            record_abi_limit(resolve, self.params.types(), MAX_FLAT_PARAMS).flattened,
            record_abi_limit(resolve, self.results.types(), MAX_FLAT_RESULTS).flattened,
        )
    }

    fn core_type(&self, resolve: &Resolve) -> (Vec<ValType>, Vec<ValType>) {
        match self.kind {
            FunctionKind::Export => self.canonical_core_type(resolve),
            FunctionKind::Import | FunctionKind::ExportLift | FunctionKind::ExportLower => (
                vec![VecType::I32, VecType::I32, VecType::I32, VecType::I32],
                Vec::new(),
            ),
            FunctionKind::ExportPostReturn => (
                record_abi_limit(resolve, self.results.types(), MAX_FLAT_RESULTS).flattened,
                Vec::new(),
            ),
        }
    }

    fn is_dispatchable(&self) -> bool {
        match self.kind {
            Function::Import | FunctionKind::ExportLift | FunctionKind::ExportLower => true,
            FunctionKind::Export | FunctionKind::ExportPostReturn => false,
        }
    }
}

struct Summary<'a> {
    resolve: &'a Resolve,
    functions: Vec<MyFunction<'a>>,
    types: IndexSet<TypeId>,
    imported_interfaces: HashMap<InterfaceId, &'a str>,
    exported_interfaces: HashMap<InterfaceId, &'a str>,
}

impl<'a> Summary<'a> {
    fn try_new(resolve: &'a Resolve) -> Result<Self> {
        let mut me = Self {
            resolve,
            functions: Vec::new(),
            types: IndexMap::new(),
        };

        me.visit_functions(&resolve.worlds[world].imports, Direction::Import)?;
        me.visit_functions(&resolve.worlds[world].exports, Direction::Export)?;

        Ok(me)
    }

    fn visit_type(&mut self, ty: Type) {
        match ty {
            Type::Bool
            | Type::U8
            | Type::U16
            | Type::U32
            | Type::S8
            | Type::S16
            | Type::S32
            | Type::Char
            | Type::U64
            | Type::S64
            | Type::Float32
            | Type::Float64
            | Type::String
            | Type::Id(id) => match self.resolve.types[id].kind {
                TypeDefKind::Record(record) => {
                    self.types.insert(id);
                    for field in &record.fields {
                        self.visit_type(field.ty);
                    }
                }
                TypeDefKind::List(ty) => {
                    self.visit_type(ty);
                }
                _ => todo!(),
            },
        }
    }

    fn visit_function(
        &mut self,
        interface: Option<&'a str>,
        name: &'a str,
        params: &'a [(String, Type)],
        results: &'a Results,
        direction: Direction,
    ) {
        for ty in params.types() {
            self.visit_type(ty);
        }

        for ty in results.types() {
            self.visit_type(ty);
        }

        let make = |kind| MyFunction {
            kind,
            interface,
            name,
            params,
            results,
        };

        match direction {
            Direction::Import => {
                self.functions.push(make(FunctionKind::Import));
            }
            Direction::Export => {
                // NB: We rely on this order when compiling, so please don't change it:
                // todo: make this less fragile
                self.functions.push(make(FunctionKind::Export));
                self.functions.push(make(FunctionKind::ExportLift));
                self.functions.push(make(FunctionKind::ExportLower));
                self.functions.push(make(FunctionKind::ExportPostReturn));
            }
        }
    }

    fn visit_functions(
        &mut self,
        items: &'a IndexMap<String, WorldItem>,
        direction: Direction,
    ) -> Result<()> {
        for (item_name, item) in items {
            match item {
                WorldItem::Interface(interface) => {
                    match direction {
                        Direction::Import => self.imported_interfaces.insert(interface, item_name),
                        Direction::Export => self.exported_interfaces.insert(interface, item_name),
                    }
                    let interface = &self.resolve.interfaces[interface];
                    for (func_name, func) in interface.functions {
                        self.visit_function(
                            Some(&interface.name),
                            func_name,
                            &func.params,
                            &func.results,
                            direction,
                        );
                    }
                }

                WorldItem::Function(func) => {
                    self.visit_func(None, &func.name, &func.params, &func.results, direction);
                }

                WorldItem::Type(_) => bail!("type imports and exports not yet supported"),
            }
        }
        Ok(())
    }

    fn collect_symbols(&self) -> Symbols<'a> {
        let mut imports = Vec::new();
        let mut exports = Vec::new();
        for function in self.functions {
            match function.kind {
                FunctionKind::Import => imports.push(symbols::Function {
                    interface: function.interface,
                    name: function.name,
                }),
                FunctionKind::Export => exports.push(symbols::Function {
                    interface: function.interface,
                    name: function.name,
                }),
                _ => (),
            }
        }

        let mut types = Vec::new();
        for ty in self.types {
            let ty = &self.resolve.types[ty];
            if let TypeOwner::Interface(interface) = ty.owner {
                let (direction, interface) =
                    if let Some(name) = self.imported_interfaces.get(interface) {
                        (Direction::Import, name)
                    } else {
                        (Direction::Export, self.exported_interfaces[interface])
                    };

                types.push(symbols::Type {
                    direction,
                    interface,
                    name: ty.name.as_deref(),
                });
            } else {
                todo!("handle types exported directly from a world");
            };
        }
    }

    fn generate_code(&self, path: &Path) -> Result<()> {
        let mut interface_imports = HashMap::new();
        let mut interface_exports = HashMap::new();
        let mut world_imports = Vec::new();
        let mut index = 0;
        for function in self.functions {
            match function.kind {
                FunctionKind::Import => {
                    // todo: generate typings
                    let snake = function.name.to_snake_case();

                    let params = function
                        .params
                        .iter()
                        .map(|(name, _)| name)
                        .collect::<Vec<_>>()
                        .join(", ");

                    let result_count = function.results.types().count();

                    let code = format!(
                        "def {snake}({params}):\n    \
                         return componentize_py.call_import({index}, [{params}], {result_count})\n\n"
                    );

                    if let Some(interface) = function.interface {
                        interface_imports.entry(interface).or_default().push(code);
                    } else {
                        world_imports.push(code);
                    }
                }
                // todo: generate `Protocol` for each exported function
                _ => (),
            }

            if function.is_dispatchable() {
                index += 1;
            }
        }

        for (index, ty) in self.types.iter().enumerate() {
            let ty = &self.resolve.types[ty];
            if let TypeOwner::Interface(interface) = ty.owner {
                // todo: generate `dataclass` with typings
                let camel = || {
                    if let Some(name) = ty.name {
                        name.to_upper_camel_case()
                    } else {
                        format!("AnonymousType{index}")
                    }
                };

                let code = match ty.kind {
                    TypeDefKind::Record(record) => {
                        let camel = camel();

                        let snakes =
                            || record.fields.iter().map(|field| field.name.to_snake_case());

                        let params = iter::once("self".to_owned())
                            .chain(snakes())
                            .collect::<Vec<_>>()
                            .join(", ");

                        let mut inits = snakes()
                            .map(|snake| format!("self.{snake} = {snake}"))
                            .collect::<Vec<_>>()
                            .join("\n        ");

                        if inits.is_empty() {
                            inits = "pass".to_owned()
                        }

                        Some(format!(
                            "class {camel}:\n    \
                             def __init__({params}):\n        \
                             {inits}\n\n"
                        ))
                    }
                    TypeDefKind::List(_) => None,
                    _ => todo!(),
                };

                if let Some(code) = code {
                    if let Some(name) = self.imported_interfaces.get(interface) {
                        interface_imports.entry(name).or_default().push(code)
                    } else {
                        interface_exports
                            .entry(self.exported_interfaces[interface])
                            .or_default()
                            .push(code)
                    }
                }
            } else {
                todo!("handle types exported directly from a world");
            };
        }

        if !interface_imports.is_empty() {
            let dir = path.join("imports");
            fs::create_dir_all(&dir)?;

            for (name, code) in interface_imports {
                let file = File::create(dir.join(name))?;
                for code in code {
                    file.write_all(code.as_bytes())?;
                }
            }

            File::create(dir.join("__init__.py"))?;
        }

        if !interface_exports.is_empty() {
            let dir = path.join("exports");
            fs::create_dir_all(&dir)?;

            for (name, code) in interface_exports {
                let file = File::create(dir.join(name))?;
                for code in code {
                    file.write_all(code.as_bytes())?;
                }
            }

            File::create(dir.join("__init__.py"))?;
        }

        if !world_imports.is_empty() {
            let file = File::create(path.join("__init__.py"))?;
            for code in world_imports {
                file.write_all(code.as_bytes())?;
            }
        }
    }
}

struct Visitor<F> {
    remap: F,
    buffer: Vec<u8>,
}

// Adapted from https://github.com/bytecodealliance/wasm-tools/blob/1e0052974277b3cce6c3703386e4e90291da2b24/crates/wit-component/src/gc.rs#L1118
macro_rules! define_encode {
    ($(@$p:ident $op:ident $({ $($arg:ident: $argty:ty),* })? => $visit:ident)*) => {
        $(
            #[allow(clippy::drop_copy)]
            fn $visit(&mut self $(, $($arg: $argty),*)?)  {
                #[allow(unused_imports)]
                use wasm_encoder::Instruction::*;
                $(
                    $(
                        let $arg = define_encode!(map self $arg $arg);
                    )*
                )?
                let insn = define_encode!(mk $op $($($arg)*)?);
                insn.encode(&mut self.buf);
            }
        )*
    };

    // No-payload instructions are named the same in wasmparser as they are in
    // wasm-encoder
    (mk $op:ident) => ($op);

    // Instructions which need "special care" to map from wasmparser to
    // wasm-encoder
    (mk BrTable $arg:ident) => ({
        BrTable($arg.0, $arg.1)
    });
    (mk CallIndirect $ty:ident $table:ident $table_byte:ident) => ({
        drop($table_byte);
        CallIndirect { ty: $ty, table: $table }
    });
    (mk ReturnCallIndirect $ty:ident $table:ident) => (
        ReturnCallIndirect { ty: $ty, table: $table }
    );
    (mk MemorySize $mem:ident $mem_byte:ident) => ({
        drop($mem_byte);
        MemorySize($mem)
    });
    (mk MemoryGrow $mem:ident $mem_byte:ident) => ({
        drop($mem_byte);
        MemoryGrow($mem)
    });
    (mk I32Const $v:ident) => (I32Const($v));
    (mk I64Const $v:ident) => (I64Const($v));
    (mk F32Const $v:ident) => (F32Const(f32::from_bits($v.bits())));
    (mk F64Const $v:ident) => (F64Const(f64::from_bits($v.bits())));
    (mk V128Const $v:ident) => (V128Const($v.i128()));

    // Catch-all for the translation of one payload argument which is typically
    // represented as a tuple-enum in wasm-encoder.
    (mk $op:ident $arg:ident) => ($op($arg));

    // Catch-all of everything else where the wasmparser fields are simply
    // translated to wasm-encoder fields.
    (mk $op:ident $($arg:ident)*) => ($op { $($arg),* });

    // Individual cases of mapping one argument type to another
    (map $self:ident $arg:ident memarg) => {IntoMemArg($arg).into()};
    (map $self:ident $arg:ident blockty) => {IntoBlockType($arg).into()};
    (map $self:ident $arg:ident hty) => {IntoHeapType($arg).into()};
    (map $self:ident $arg:ident tag_index) => {$arg};
    (map $self:ident $arg:ident relative_depth) => {$arg};
    (map $self:ident $arg:ident function_index) => {($self.remap)($arg)};
    (map $self:ident $arg:ident global_index) => {$arg};
    (map $self:ident $arg:ident mem) => {$arg};
    (map $self:ident $arg:ident src_mem) => {$arg};
    (map $self:ident $arg:ident dst_mem) => {$arg};
    (map $self:ident $arg:ident table) => {$arg};
    (map $self:ident $arg:ident table_index) => {$arg};
    (map $self:ident $arg:ident src_table) => {$arg};
    (map $self:ident $arg:ident dst_table) => {$arg};
    (map $self:ident $arg:ident type_index) => {$arg};
    (map $self:ident $arg:ident ty) => {IntoValType($arg).into()};
    (map $self:ident $arg:ident local_index) => {$arg};
    (map $self:ident $arg:ident lane) => {$arg};
    (map $self:ident $arg:ident lanes) => {$arg};
    (map $self:ident $arg:ident elem_index) => {$arg};
    (map $self:ident $arg:ident data_index) => {$arg};
    (map $self:ident $arg:ident table_byte) => {$arg};
    (map $self:ident $arg:ident mem_byte) => {$arg};
    (map $self:ident $arg:ident value) => {$arg};
    (map $self:ident $arg:ident targets) => ((
        $arg.targets().map(|i| i.unwrap()).collect::<Vec<_>>().into(),
        $arg.default(),
    ));
}

impl<'a, F: Fn(u32) -> u32> VisitOperator<'a> for Visitor<F> {
    type Output = ();

    wasmparser::for_each_operator!(define_encode);
}

fn componentize(
    module: &[u8],
    resolve: &Resolve,
    world: WorldId,
    summary: &Summary,
) -> Result<Vec<u8>> {
    // First pass: find stack pointer and dispatch function, and count various items
    let mut types = None;
    let mut import_count = None;
    let mut dispatch_import_index = None;
    let mut dispatch_type_index = None;
    let mut function_count = None;
    let mut table_count = None;
    let mut global_count = None;
    let mut stack_pointer_index = None;
    for payload in Parser::new(0).parse_all(module) {
        match payload? {
            Payload::TypeSection(reader) => {
                types = Some(reader.into_iter().collect::<Vec<_>>());
            }
            Payload::ImportSection(reader) => {
                let count = 0;
                for import in reader {
                    let import = import?;
                    if import.module == "componentize-py" {
                        if import.field == "dispatch" {
                            match import.ty {
                                TypeRef::Func(ty) if types[ty] == dispatch_type => {
                                    dispatch_import_index = Some(index);
                                    dispatch_type_index = Some(ty);
                                }
                                _ => bail!(
                                    "componentize-py#dispatch has incorrect type: {:?}",
                                    import.ty
                                ),
                            }
                        } else {
                            bail!(
                                "componentize-py module import has unrecognized name: {}",
                                import.field
                            );
                        }
                    }
                }
                import_count = Some(count)
            }
            Payload::FunctionSection(reader) => {
                function_count = Some(reader.into_iter().count() + import_count.unwrap())
            }
            Payload::TableSection(reader) => {
                table_count = Some(reader.into_iter().count());
            }
            Payload::GlobalSection(reader) => {
                global_count = Some(reader.into_iter().count());
            }
            Payload::CustomSection(section) if section.name() == "name" => {
                let subsections = NameSectionReader::new(section.data(), section.data_offset());
                for subsection in subsections {
                    match subsection? {
                        Name::Global(map) => {
                            for naming in map {
                                let naming = naming?;
                                if naming.name == "__stack_pointer" {
                                    stack_pointer_index = Some(naming.index);
                                    break;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }

            _ => {}
        }
    }

    let old_import_count = import_count.unwrap();
    let old_function_count = function_count.unwrap();
    let new_import_count = summary
        .functions
        .iter()
        .filter(|f| matches!(f, FunctionKind::Import(_)))
        .count();
    let dispatchable_function_count = summary
        .functions
        .iter()
        .filter(|f| f.is_dispatchable())
        .count();
    let dispatch_type_index = dispatch_type_index.unwrap();

    let remap = move |index| match index.cmp(dispatch_import_index) {
        Ordering::Less => index,
        Ordering::Equal => old_function_count + new_import_count - 1,
        Ordering::Greater => {
            if index < old_import_count {
                index - 1
            } else {
                old_import_count + new_import_count - 1
            }
        }
    };

    let mut link_name_map = LINK_LIST
        .iter()
        .map(|&v| (v, format!("{v:?}")))
        .collect::<HashMap<_>>();

    let mut link_map = HashMap::new();

    let mut result = Module::new();
    let mut code_entries_remaining = old_function_count - old_import_count;
    let mut code_section = CodeSection::new();

    for payload in Parser::new(0).parse_all(module) {
        match payload? {
            Payload::TypeSection(reader) => {
                let mut types = TypeSection::new();
                for wasmparser::Type::Func(ty) in types {
                    let map = |&ty| IntoValType(ty).into();
                    types.function(ty.params().iter().map(map), ty.params().iter().map(map));
                }
                // TODO: should probably deduplicate these types:
                for function in summary
                    .functions
                    .iter()
                    .filter(|f| matches!(f.kind, FunctionKind::Import))
                {
                    let (params, results) = function.canonical_core_type(resolve);
                    types.function(params, results);
                }
                for function in &summary.functions {
                    let (params, results) = function.core_type(resolve);
                    types.function(params, results);
                }
                result.section(&types);
            }

            Payload::ImportSection(reader) => {
                let mut imports = ImportSection::new();
                for import in reader
                    .into_iter()
                    .enumerate()
                    .filter_map(|(index, import)| {
                        (index == dispatch_import_index).then_some(import)
                    })
                {
                    let import = import?;
                    imports.import(import.module, import.field, IntoEntityType(import.ty));
                }
                for (index, function) in summary
                    .functions
                    .iter()
                    .filter(|f| matches!(f.kind, FunctionKind::Import))
                    .enumerate()
                {
                    imports.import(
                        function.interface.unwrap_or(&resolve.worlds[world].name),
                        function.name,
                        EntityType::Function(old_type_count + index),
                    );
                }
                result.section(&imports);
            }

            Payload::FunctionSection(reader) => {
                let mut functions = FunctionSection::new();
                for ty in reader {
                    functions.function(ty?);
                }
                // dispatch function:
                functions.function(dispatch_type_index);
                for index in 0..summary.functions.len() {
                    functions.function(old_type_count + new_import_count + index);
                }
                result.section(&functions);
            }

            Payload::TableSection(reader) => {
                let mut tables = TableSection::new();
                for table in reader {
                    result.table(IntoTableType(table?).into());
                }
                result.table(TableType {
                    element_type: RefType {
                        nullable: true,
                        heap_type: HeapType::TypedFunc(dispatch_type_index),
                    },
                    minimum: dispatchable_function_count,
                    maximum: Some(dispatchable_function_count),
                });
                result.section(&tables);
            }

            Payload::GlobalSection(reader) => {
                let mut globals = GlobalSection::new();
                for global in reader {
                    globals.global(
                        IntoGlobalType(global.ty).into(),
                        &IntoConstExpr(global.init_expr).into(),
                    );
                }
                globals.global(
                    GlobalType {
                        val_type: ValType::I32,
                        mutable: true,
                    },
                    &ConstExpr::i32_const(0),
                );
                result.section(&globals);
            }

            Payload::ExportSection(reader) => {
                let mut exports = ExportSection::new();
                for export in reader {
                    let export = export?;
                    if let Some(name) = export.name.strip_prefix("componentize-py#") {
                        if let Some(link) = link_name_map.remove(name) {
                            if let ExternalKind::Func = export.kind {
                                link_map.insert(link, remap(export.index));
                            } else {
                                bail!("unexpected kind for {}: {:?}", export.name, export.kind);
                            }
                        } else {
                            bail!("duplicate or unrecognized export name: {}", export.name);
                        }
                    } else {
                        exports.export(
                            export.name,
                            IntoExportKind(export.kind).into(),
                            remap(export.index),
                        );
                    }
                }

                if !link_name_map.is_empty() {
                    bail!("missing expected exports: {:#?}", link_name_map.keys());
                }

                for (index, function) in summary.functions.enumerate() {
                    if let FunctionKind::Export = function.kind {
                        exports.export(
                            if let Some(interface) = function.interface {
                                &format!("{}#{}", interface, function.name)
                            } else {
                                function.name
                            },
                            ExportKind::Func,
                            old_function_count + new_import_count + index,
                        );
                    }
                }
                result.section(&exports);
            }

            Payload::CodeSectionEntry(body) => {
                let reader = body.get_binary_reader();
                let mut locals = Vec::new();
                for _ in 0..reader.read_var_u32()? {
                    let count = reader.read_var_u32()?;
                    let ty = reader.read()?;
                    locals.push((count, ty));
                }

                let visitor = Visitor {
                    remap,
                    buffer: Vec::new(),
                };
                while !reader.eof() {
                    reader.visit_operator(&mut visitor)?;
                }

                let function = Function::new(locals);
                function.raw(visitor.buffer);
                code_section.function(&function);

                *code_entries_remaining = (*code_entries_remaining).checked_sub(1);
                if *code_entries_remaining == 0 {
                    let exports = summary
                        .functions
                        .filter_map(|f| {
                            if let FunctionKind::Export = f.kind {
                                Some((function.interface, function.name))
                            } else {
                                None
                            }
                        })
                        .collect::<IndexSet<_>>();

                    let import_index = 0;
                    let dispatch_index = 0;
                    for function in &summary.functions {
                        let gen = FunctionBindgen::new(
                            resolve,
                            stack_pointer_index,
                            &link_map,
                            &summary.types,
                            function.params,
                            function.results,
                        );

                        match function.kind {
                            FunctionKind::Import => {
                                gen.compile_import(old_import_count - 1 + import_index);
                                import_index += 1;
                            }
                            FunctionKind::Export => gen.compile_export(
                                exports
                                    .get_index_of(&(function.interface, function.name))
                                    .unwrap()
                                    .try_into()?,
                                // next two `dispatch_index`es should be the lift and lower functions (see ordering
                                // in `Summary::visit_function`):
                                dispatch_index,
                                dispatch_index + 1,
                            ),
                            FunctionKind::ExportLift => function.compile_export_lift(),
                            FunctionKind::ExportLower => function.compile_export_lower(),
                            FunctionKind::ExportPostReturn => gen.compile_export_post_return(),
                        };

                        let func = Function::new_with_locals_types(gen.local_types);
                        for instruction in &gen.instructions {
                            func.instruction(instruction);
                        }
                        code_section.function(&func);

                        if function.is_dispatchable() {
                            dispatch_index += 1;
                        }
                    }

                    let dispatch = Function::new([]);

                    dispatch.instruction(&Ins::GlobalGet(table_count));
                    dispatch.instruction(&Ins::If(BlockType::Empty));
                    dispatch.instruction(&Ins::I32Const(0));
                    dispatch.instruction(&Ins::GlobalSet(table_count));

                    let table_index = 0;
                    for (index, function) in summary.functions.iter().enumarate() {
                        if function.is_dispatchable() {
                            dispatch.instruction(&Ins::RefFunc(
                                old_function_count + new_import_count + index,
                            ));
                            dispatch.instruction(&Ins::I32Const(table_index));
                            dispatch.instruction(&Ins::TableSet(table));
                            table_index += 1;
                        }
                    }

                    dispatch.instruction(&Ins::End);

                    let dispatch_param_count = 4;
                    for local in 0..dispatch_param_count {
                        dispatch.instruction(&Ins::LocalGet(local));
                    }
                    dispatch.instruction(&Ins::CallIndirect(local));

                    code_section.function(&dispatch);

                    result.section(&code_section);
                }
            }

            Payload::CustomSection(section) if section.name() == "name" => {
                let mut func_names = Vec::new();
                let mut global_names = Vec::new();

                let subsections = NameSectionReader::new(section.data(), section.data_offset());
                for subsection in subsections {
                    match subsection? {
                        Name::Function(map) => {
                            for naming in map {
                                let naming = naming?;
                                function_names.push((remap(naming.index), naming.name));
                            }
                        }
                        Name::Global(map) => {
                            for naming in map {
                                let naming = naming?;
                                global_names.push((naming.index, naming.name));
                            }
                        }
                        // TODO: do we want to copy over other names as well?
                        _ => {}
                    }
                }

                global_names.push((table_init_index.unwrap(), "componentize-py#table_init"));

                function_names.push((
                    old_function_count + new_import_count - 1,
                    "componentize-py#dispatch",
                ));

                for (index, function) in summary
                    .functions
                    .iter()
                    .filter(|f| matches!(f.kind, FunctionKind::Import(_)))
                    .enumerate()
                {
                    function_names.push((
                        old_import_count - 1 + index,
                        format!("{}-import", function.internal_name()),
                    ));
                }

                for (index, function) in summary.functions.iter().enumerate() {
                    function_names.push((
                        old_function_count + new_import_count + index,
                        function.internal_name(),
                    ));
                }

                let mut data = Vec::new();
                for (code, names) in [(0x01_u8, &function_names), (0x07_u8, &global_names)] {
                    let mut subsection = Vec::new();
                    names.len().encode(&mut subsection);
                    for (index, name) in names {
                        index.encode(&mut subsection);
                        name.encode(&mut subsection);
                    }
                    section.push(code);
                    subsection.encode(&mut data);
                }

                result.section(&CustomSection {
                    name: "name",
                    data: &data,
                });
            }

            payload => {
                if let Some((id, range)) = payload.as_section() {
                    result.section(&RawSection {
                        id,
                        data: &module[range],
                    });
                }
            }
        }
    }

    result.section(&CustomSection {
        name,
        data: &metadata::encode(
            &bindgen.resolve,
            world,
            wit_component::StringEncoding::UTF8,
            None,
        )?,
    });

    // Encode with WASI Preview 1 adapter
    Ok(ComponentEncoder::default()
        .validate(true)
        .module(&result.encode())?
        .adapter(
            "wasi_snapshot_preview1",
            &zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/wasi_snapshot_preview1.wasm.zst"
            ))))?,
        )?
        .encode()?)
}

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

        let component = componentize(
            &module,
            &parse_wit(&options.wit_path, options.wit_world.as_deref())?,
        )?;

        fs::write(&options.output, component)?;
    } else {
        let options = Options::parse();

        let temp = tempfile::tempdir()?;

        Archive::new(Decoder::new(Cursor::new(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/python-lib.tar.zst"
        ))))?)
        .unpack(temp.path())?;

        let mut python_path = options.python_path;
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

        let summary = summarize(&parse_wit(&options.wit_path, options.wit_world.as_deref())?)?;
        bincode::serialize_into(&mut stdin, &summary)?;
        stdin.rewind()?;

        let mut cmd = Command::new(env::args().next().unwrap());
        cmd.env_clear()
            .env("COMPONENTIZE_PY_WIZEN", "1")
            .arg(&options.app_name)
            .arg(&options.wit_path)
            .arg(
                temp.path()
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

fn generate_bindings((resolve, world): &(Resolve, WorldId)) -> Result<Bindings> {
    // Generate a Python script which declares the types and the imports (which pass their arguments in an array to
    // a low-level `call_import` function defined in Rust, which in turn marshals them using the canonical ABI and
    // calls the real `call_import`) using a factored-out version of `wasmtime-py`'s `InterfaceGenerator`.
    // `call_import` should take a `pyo3::Python` and a slice of `&PyAny`s.
    //
    // Could hard-code this for binding testing!

    // Then, build `Vec<String>`s for imports, exports, and types.  We'll refer to the functions and types by
    // indexes into those arrays in the generated code below.

    // Finally, generate Wasm functions for each import and export which lift to and lower from `&PyAny`s.  For
    // exports, we start by loading the arguments into a stack-based array and passing control to Rust, which will
    // call back with the Python GIL into a generated function which does argument lifting, then calls the Python
    // function, and finally calls another generated function to do result lowering, returning the result back to
    // the original function.

    let mut gen = WorldBindgen {
        resolve: &resolve,
        types: Vec::new(),
        type_map: HashMap::new(),
        imports: Vec::new(),
        exports: Vec::new(),
    };
    gen.visit_items(&resolve.worlds[world].imports, Direction::Import)?;
    gen.visit_items(&resolve.worlds[world].exports, Direction::Export)?;

    // Use a single dispatch function and function table for imports, export lifts, and export lowers, since
    // they'll all have the same core type.

    // let dispatch = {
    //     let mut gen = FunctionBindgen::new(gen);

    //     gen.push(Ins::LocalGet(0));
    //     gen.push(Ins::LocalGet(1));
    //     gen.push(Ins::LocalGet(2));
    //     gen.push(Ins::LocalGet(3));
    //     gen.push(Ins::CallIndirect { ty: todo!(), table: todo!() });
    // };

    // Also, define a table init fuction which initializes the function table.

    Ok(gen.build())
}

impl WorldBindgen {
    fn visit_items(
        &mut self,
        items: &IndexMap<String, WorldItem>,
        direction: Direction,
    ) -> Result<()> {
        for (item_name, item) in items {
            match item {
                WorldItem::Interface(interface) => {
                    for (func_name, func) in &resolve.interfaces[interface].functions {
                        self.visit_func(
                            &format!("{item_name}#{func_name}"),
                            &func.params,
                            &func.results,
                            direction,
                        );
                    }
                }

                WorldItem::Function(func) => {
                    self.visit_func(&func.name, &func.params, &func.results, direction)
                }

                WorldItem::Type(_) => bail!("type imports and exports not yet supported"),
            }
        }
        Ok(())
    }

    fn visit_func(
        &mut self,
        name: &str,
        params: &[(String, Type)],
        results: &Results,
        direction: Direction,
    ) {
        match direction {
            Direction::Import => {
                let index = self.imports.len();
                let func = self.generate_import(index, params, results);
                self.imports.push((name.to_owned(), func));
            }
            Direction::Export => {
                let index = self.exports.len();
                let entry = self.generate_export_entry(index, params, results);
                let lift = self.generate_export_lift(params);
                let lower = self.generate_export_lower(results);
                let post_return = self.maybe_generate_export_post_return(results);

                self.exports.push((
                    name.to_owned(),
                    Export {
                        entry,
                        lift,
                        lower,
                        post_return,
                    },
                ));
            }
        }
    }

    fn generate_import(
        &mut self,
        index: usize,
        params: &[(String, Type)],
        results: &Results,
    ) -> Function {
        // Arg 0: *const Python
        let context = 0;
        // Arg 1: *const &PyAny
        let input = 1;
        // Arg 2: *mut &PyAny
        let output = 2;

        let params_flattened = self.flatten_all(params.iter().map(|(_, ty)| *ty));
        let params_abi = self.record_abi(params.iter().map(|(_, ty)| *ty));
        let results_flattened = self.flatten_all(results.iter().map(|ty| *ty));
        let results_abi = self.record_abi(results.iter().map(|ty| *ty));

        let mut gen = FunctionBuilder::new(self);

        let locals = if params_flattened.len() <= MAX_FLAT_PARAMS {
            let locals = params_flattened
                .iter()
                .map(|ty| {
                    let local = gen.push_local(ty);
                    gen.push(Ins::LocalSet(local));
                    local
                })
                .collect::<Vec<_>>();

            let mut load_offset = 0;
            for (_, ty) in params {
                let value = self.push_local(CoreType::I32);

                gen.push(Ins::LocalGet(context));
                gen.push(Ins::LocalGet(input));
                gen.push(Ins::I32Load(mem_arg(load_offset, WORD_ALIGN)));
                gen.push(Ins::LocalSet(value));

                gen.lower(ty, context, value);

                for local in locals[lift_index..][..flat_count] {
                    gen.push(Ins::LocalTee(local));
                }

                load_offset += WORD_SIZE;

                self.pop_local(value);
            }

            Some(locals)
        } else {
            gen.push_stack(params_abi.size);

            let mut store_offset = 0;
            for (_, ty) in params {
                let value = self.push_local(CoreType::I32);
                let destination = self.push_local(CoreType::I32);

                let abi = self.abi(ty);
                align(&mut store_offset, abi.align);

                gen.get_stack();
                gen.push(Ins::I32Const(store_offset));
                gen.push(Ins::I32Add);
                gen.push(Ins::LocalSet(destination));

                gen.push(Ins::LocalGet(input));
                gen.push(Ins::I32Load(mem_arg(load_offset, WORD_ALIGN)));
                gen.push(Ins::LocalSet(value));

                gen.store(ty, context, value, destination);

                store_offset += abi.size;

                self.pop_local(destination);
                self.pop_local(value);
            }

            gen.get_stack();

            None
        };

        if results_flattened.len() > MAX_FLAT_RESULTS {
            gen.push_stack(results_abi.size);

            gen.get_stack();
        }

        gen.call(Call::Import(index));

        if results_flattened.len() <= MAX_FLAT_RESULTS {
            let locals = results_flattened
                .iter()
                .map(|ty| {
                    let local = gen.push_local(ty);
                    gen.push(Ins::LocalSet(local));
                    local
                })
                .collect::<Vec<_>>();

            gen.lift_record(results.iter(), context, &locals, output);

            for (local, ty) in locals.iter().zip(&results_flattened).rev() {
                gen.pop_local(local, ty);
            }
        } else {
            let source = self.push_local(CoreType::I32);

            self.get_stack();
            self.push(Ins::LocalSet(source));

            self.load_record(results.iter(), context, source, output);

            self.pop_local(source, CoreType::I32);
            gen.pop_stack(results_abi.size);
        }

        if let Some(locals) = locals {
            gen.free_lowered_record(params.iter().map(|(_, ty)| *ty), &locals);

            for (local, ty) in locals.iter().zip(&params_flattened).rev() {
                gen.pop_local(local, ty);
            }
        } else {
            let value = self.push_local(CoreType::I32);

            self.get_stack();
            self.push(Ins::LocalSet(value));

            gen.free_stored_record(params.iter().map(|(_, ty)| *ty), value);

            self.pop_local(value, CoreType::I32);
            gen.pop_stack(params_abi.size);
        }
    }

    fn generate_export_entry(
        &mut self,
        index: usize,
        params: &[(String, Type)],
        results: &Results,
    ) -> Function {
        gen.call(Call::InitFunctionTable);

        let params_flattened = self.flatten_all(params.iter().map(|(_, ty)| *ty));
        let params_abi = self.record_abi(params.iter().map(|(_, ty)| *ty));
        let results_flattened = self.flatten_all(results.iter().map(|ty| *ty));
        let results_abi = self.results_abi(results.iter().map(|ty| *ty));

        let mut gen = FunctionBuilder::new(self);

        let param_flat_count = if params_flattened.len() <= MAX_FLAT_PARAMS {
            gen.push_stack(params_abi.size);

            let destination = self.push_local(CoreType::I32);
            gen.get_stack();
            gen.push(Ins::LocalSet(destination));

            store_copy_record(
                params.iter().map(|(_, ty)| *ty),
                &(0..params_flattened.len()).collect::<Vec<_>>(),
                destination,
            );

            self.pop_local(destination);

            gen.get_stack();

            params_flattened.len()
        } else {
            gen.push(Ins::LocalGet(0));

            1
        };

        if results_flattened.len() <= MAX_FLAT_RESULTS {
            gen.push_stack(results_abi.size);

            gen.get_stack();
        } else {
            gen.push(Ins::LocalGet(param_flat_count));
        }

        gen.call(Call::Export(index));

        if results_flattened.len() <= MAX_FLAT_RESULTS {
            let source = self.push_local(CoreType::I32);
            gen.get_stack();
            gen.push(Ins::LocalSet(source));

            self.load_copy_record(results.iter(), source);

            self.pop_local(source);

            gen.pop_stack(results_abi.size);
        }

        if params_flattened.len() <= MAX_FLAT_PARAMS {
            gen.pop_stack(params_abi.size);
        }
    }

    fn generate_export_lift(&mut self, params: &[(String, Type)]) -> Function {
        // Arg 0: *const Python
        let context = 0;
        // Arg 1: *const MyParams
        let source = 1;
        // Arg 2: *mut [&PyAny]
        let destination = 2;

        let mut gen = FunctionBuilder::new(self);

        gen.load_record(
            params.iter().map(|(_, ty)| *ty),
            context,
            source,
            destination,
        );

        gen.build()
    }

    fn generate_export_lower(&mut self, results: &Results) -> Function {
        // Arg 0: *const Python
        let context = 0;
        // Arg 1: *const [&PyAny]
        let source = 1;
        // Arg 2: *mut MyResults
        let destination = 2;

        let mut gen = FunctionBuilder::new(self);

        gen.store_record(results.iter(), context, source, destination);

        gen.build()
    }

    fn maybe_generate_export_post_return(&mut self, results: &Results) -> Option<Function> {
        let results_flattened = self.flatten_all(results.iter().map(|ty| *ty));

        if results_flattened.len() > MAX_FLAT_RESULTS {
            // Arg 0: *mut MyResults
            let value = 0;
            let results_abi = self.record_abi(results.iter().map(|ty| *ty));

            let mut gen = FunctionBuilder::new(self);

            gen.free_stored_record(results.iter(), value);

            gen.push(Ins::LocalGet(value));
            gen.push(Ins::I32Const(results_abi.size));
            gen.push(Ins::I32Const(results_abi.align));
            gen.call(Call::Free);

            Some(gen.build())
        } else {
            // As of this writing, no type involving heap allocation can fit into `MAX_FLAT_RESULTS`, so nothing to
            // do.  We'll need to revisit this if `MAX_FLAT_RESULTS` changes or if new types are added.
            None
        }
    }
}

impl FunctionBindgen {
    fn push_stack(&mut self, size: usize) {
        self.stack_refs.push(self.push(Ins::GlobalGet(0)));
        self.push(Ins::I32Const(align(size, WORD_SIZE)));
        self.push(Ins::I32Sub);
        self.stack_refs.push(self.push(Ins::GlobalSet(0)));
    }

    fn pop_stack(&mut self, size: usize) {
        self.stack_refs.push(self.push(Ins::GlobalGet(0)));
        self.push(Ins::I32Const(align(size, WORD_SIZE)));
        self.push(Ins::I32Add);
        self.stack_refs.push(self.push(Ins::GlobalSet(0)));
    }

    fn push(&mut self, instruction: Ins) -> usize {
        gen.instructions.index_and_push(Ins::LocalGet(0))
    }

    fn call(&mut self, call: Call) {
        self.func_refs.push((call, self.push(Ins::Call(0))));
    }

    fn get_stack(&mut self) {
        gen.stack_refs.push(gen.push(Ins::GlobalGet(0)));
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
                self.call(Call::LowerI32);
            }
            Type::U64 | Type::S64 => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value));
                self.call(Call::LowerI64);
            }
            Type::Float32 => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value));
                self.call(Call::LowerF32);
            }
            Type::Float64 => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value));
                self.call(Call::LowerF64);
            }
            Type::String => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value));
                self.push_stack(WORD_SIZE * 2);
                self.call(Call::LowerString);
                self.stack();
                self.push(Ins::I32Load(mem_arg(0, WORD_ALIGN)));
                self.stack();
                self.push(Ins::I32Load(mem_arg(WORD_SIZE, WORD_ALIGN)));
                self.pop_stack(WORD_SIZE * 2);
            }
            Type::Id(id) => match self.gen.resolve.types[id].kind {
                TypeDefKind::Record(record) => {
                    for field in &record.fields {
                        let name = self.name(&field.name);
                        let field_value = self.push_local(CoreType::I32);

                        self.push(Ins::LocalGet(context));
                        self.push(Ins::LocalGet(value));
                        self.push(Ins::I32Const(name));
                        self.call(Call::GetField);
                        self.push(Ins::LocalSet(field_value));

                        self.lower(field.ty, context, field_value);

                        self.pop_local(field_value, CoreType::I32);
                    }
                }
                TypeDefKind::List(ty) => {
                    // TODO: optimize `list<u8>` (and others if appropriate)

                    let abi = self.gen.abi(ty);
                    let length = self.push_local(CoreType::I32);
                    let index = self.push_local(CoreType::I32);
                    let destination = self.push_local(CoreType::I32);
                    let element_value = self.push_local(CoreType::I32);
                    let element_destination = self.push_local(CoreType::I32);

                    self.push(Ins::LocalGet(context));
                    self.push(Ins::LocalGet(value));
                    self.call(Call::GetListLength);
                    self.push(Ins::LocalSet(length));

                    self.push(Ins::I32Const(0));
                    self.push(Ins::LocalSet(index));

                    self.push(Ins::LocalGet(context));
                    self.push(Ins::LocalGet(length));
                    self.push(Ins::I32Const(abi.size));
                    self.push(Ins::I32Mul);
                    self.push(Ins::I32Const(abi.align));
                    self.call(Call::Allocate);
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
                    self.call(Call::GetListElement);
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

                    self.pop_local(element_destination, CoreType::I32);
                    self.pop_local(element_value, CoreType::I32);
                    self.pop_local(destination, CoreType::I32);
                    self.pop_local(index, CoreType::I32);
                    self.pop_local(length, CoreType::I32);
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
                self.call(Call::LowerString);
            }
            Type::Id(id) => match self.gen.resolve.types[id].kind {
                TypeDefKind::Record(record) => {
                    let mut store_offset = 0;
                    for field in &record.fields {
                        let abi = self.abi(ty);
                        align(&mut store_offset, abi.align);

                        let name = self.name(&field.name);
                        let field_value = self.push_local(CoreType::I32);
                        let field_destination = self.push_local(CoreType::I32);

                        self.push(Ins::LocalGet(context));
                        self.push(Ins::LocalGet(value));
                        self.push(Ins::I32Const(name));
                        self.call(Call::GetField);
                        self.push(Ins::LocalSet(field_value));

                        self.push(Ins::LocalGet(destination));
                        self.push(Ins::I32Const(store_offset));
                        self.push(Ins::I32Add);
                        self.push(Ins::LocalSet(field_destination));

                        self.store(field.ty, context, field_value, field_destination);

                        store_offset += abi.size;

                        self.pop_local(field_destination, CoreType::I32);
                        self.pop_local(field_value, CoreType::I32);
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
            Type::Id(id) => match self.gen.resolve.types[id].kind {
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
            let abi = self.abi(ty);
            align(&mut store_offset, abi.align);

            let field_destination = self.push_local(CoreType::I32);

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

            self.pop_local(field_destination, CoreType::I32);
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
                self.call(Call::LiftI32);
            }
            Type::U64 | Type::S64 => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value[0]));
                self.call(Call::LiftI64);
            }
            Type::Float32 => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value[0]));
                self.call(Call::LiftF32);
            }
            Type::Float64 => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value[0]));
                self.call(Call::LiftF64);
            }
            Type::String => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value[0]));
                self.push(Ins::LocalGet(value[1]));
                self.call(Call::LiftString);
            }
            Type::Id(id) => match self.gen.resolve.types[id].kind {
                TypeDefKind::Record(record) => {
                    self.push_stack(record.fields.len() * WORD_SIZE);
                    let source = self.push_local(CoreType::I32);

                    self.get_stack();
                    self.push(Ins::LocalSet(source));

                    self.lift_record(record.fields.iter().map(|field| field.ty), context, source);

                    let name = self.name(&record.name);

                    self.push(Ins::I32Const(name));
                    self.get_stack();
                    self.push(Ins::I32Const(record.fields.len()));
                    self.call(Call::Init);

                    self.pop_local(source, CoreType::I32);
                    self.pop_stack(record.fields.len() * WORD_SIZE);
                }
                TypeDefKind::List(ty) => {
                    // TODO: optimize using bulk memory operation when list element is primitive

                    let source = value[0];
                    let length = value[1];

                    let abi = self.gen.abi(ty);

                    let index = self.push_local(CoreType::I32);
                    let element_source = self.push_local(CoreType::I32);

                    self.push(Ins::LocalGet(context));
                    self.call(Call::MakeList);
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

                    self.call(Call::ListAppend);

                    self.push(Ins::Br(loop_));

                    self.push(Ins::End);

                    self.push(Ins::End);
                    self.pop_block(loop_);

                    self.push(Ins::LocalGet(destination));

                    self.pop_local(element_source, CoreType::I32);
                    self.pop_local(index, CoreType::I32);
                    self.pop_local(destination, CoreType::I32);
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
            let flat_count = self.abi(ty).flat_count;

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
                let value = self.push_local(CoreType::I32);
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load8(mem_arg(0, 0)));
                self.push(Ins::LocalSet(value));
                self.lift(ty, context, &[value]);
                self.pop_local(value, CoreType::I32);
            }
            Type::U16 | Type::S16 => {
                let value = self.push_local(CoreType::I32);
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load16(mem_arg(0, 1)));
                self.push(Ins::LocalSet(value));
                self.lift(ty, context, &[value]);
                self.pop_local(value, CoreType::I32);
            }
            Type::U32 | Type::S32 | Type::Char => {
                let value = self.push_local(CoreType::I32);
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load(mem_arg(0, 2)));
                self.push(Ins::LocalSet(value));
                self.lift(ty, context, &[value]);
                self.pop_local(value, CoreType::I32);
            }
            Type::U64 | Type::S64 => {
                let value = self.push_local(CoreType::I64);
                self.push(Ins::LocalGet(source));
                self.push(Ins::I64Load(mem_arg(0, 3)));
                self.push(Ins::LocalSet(value));
                self.lift(ty, context, &[value]);
                self.pop_local(value, CoreType::I64);
            }
            Type::Float32 => {
                let value = self.push_local(CoreType::F32);
                self.push(Ins::LocalGet(source));
                self.push(Ins::F32Load(mem_arg(0, 2)));
                self.push(Ins::LocalSet(value));
                self.lift(ty, context, &[value]);
                self.pop_local(value, CoreType::F32);
            }
            Type::Float64 => {
                let value = self.push_local(CoreType::F64);
                self.push(Ins::LocalGet(source));
                self.push(Ins::F64Load(mem_arg(0, 3)));
                self.push(Ins::LocalSet(value));
                self.lift(ty, context, &[value]);
                self.pop_local(value, CoreType::F64);
            }
            Type::String => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load(mem_arg(0, WORD_ALIGN)));
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load(mem_arg(WORD_SIZE, WORD_ALIGN)));
                self.call(Call::LiftString);
            }
            Type::Id(id) => match self.gen.resolve.types[id].kind {
                TypeDefKind::Record(record) => {
                    self.push_stack(record.fields.len() * WORD_SIZE);
                    let destination = self.push_local(CoreType::I32);

                    self.get_stack();
                    self.push(Ins::LocalSet(destination));

                    self.load_record(
                        record.fields.iter().map(|field| field.ty),
                        context,
                        source,
                        destination,
                    );

                    let name = self.name(&record.name);

                    self.push(Ins::I32Const(name));
                    self.get_stack();
                    self.push(Ins::I32Const(record.fields.len()));
                    self.call(Call::Init);

                    self.pop_local(destination, CoreType::I32);
                    self.pop_stack(record.fields.len() * WORD_SIZE);
                }
                TypeDefKind::List(_) => {
                    let body = self.push_local(CoreType::I32);
                    let length = self.push_local(CoreType::I32);

                    self.push(Ins::LocalGet(source));
                    self.push(Ins::I32Load(mem_arg(0, WORD_ALIGN)));
                    self.push(Ins::LocalSet(body));

                    self.push(Ins::LocalGet(source));
                    self.push(Ins::I32Load(mem_arg(WORD_SIZE, WORD_ALIGN)));
                    self.push(Ins::LocalSet(length));

                    self.lift(ty, context, &[body, length]);

                    self.pop_local(length, CoreType::I32);
                    self.pop_local(list, CoreType::I32);
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
            let field_source = self.push_local(CoreType::I32);

            let abi = self.abi(ty);
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

            self.pop_local(field_source, CoreType::I32);
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
            Type::Id(id) => match self.gen.resolve.types[id].kind {
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
            let field_source = self.push_local(CoreType::I32);

            let abi = self.abi(ty);
            align(&mut load_offset, abi.align);

            self.push(Ins::LocalGet(source));
            self.push(Ins::I32Const(load_offset));
            self.push(Ins::I32Add);
            self.load(Ins::LocalSet(field_source));

            self.load_copy(ty, field_source);

            load_offset += abi.size;

            self.pop_local(field_source, CoreType::I32);
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
                self.call(Call::Free);
            }

            Type::Id(id) => match self.gen.resolve.types[id].kind {
                TypeDefKind::Record(record) => {
                    free_lowered_record(record.fields.iter().map(|field| field.ty), value);
                }
                TypeDefKind::List(ty) => {
                    // TODO: optimize (i.e. no loop) when list element is primitive

                    let pointer = value[0];
                    let length = value[1];

                    let abi = self.gen.abi(ty);

                    let destination = self.push_local(CoreType::I32);
                    let index = self.push_local(CoreType::I32);
                    let element_pointer = self.push_local(CoreType::I32);

                    self.push(Ins::LocalGet(context));
                    self.call(Call::MakeList);
                    self.push(Ins::LocalSet(destination));

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
                    self.call(Call::Free);

                    self.pop_local(element_pointer, CoreType::I32);
                    self.pop_local(index, CoreType::I32);
                }
                _ => todo!(),
            },
        }
    }

    fn free_lowered_record(&mut self, types: impl IntoIterator<Item = Type>, value: &[u32]) {
        let mut lift_index = 0;
        for field in &record.fields {
            let flat_count = self.abi(ty).flat_count;

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
                self.call(Call::Free);
            }

            Type::Id(id) => match self.gen.resolve.types[id].kind {
                TypeDefKind::Record(record) => {
                    free_stored_record(record.fields.iter().map(|field| field.ty), value);
                }
                TypeDefKind::List(ty) => {
                    let body = self.push_local(CoreType::I32);
                    let length = self.push_local(CoreType::I32);

                    self.push(Ins::LocalGet(value));
                    self.push(Ins::I32Load(mem_arg(0, WORD_ALIGN)));
                    self.push(Ins::LocalSet(body));

                    self.push(Ins::LocalGet(value));
                    self.push(Ins::I32Load(mem_arg(WORD_SIZE, WORD_ALIGN)));
                    self.push(Ins::LocalSet(length));

                    self.free_stored(ty, context, &[body, length]);

                    self.pop_local(length, CoreType::I32);
                    self.pop_local(list, CoreType::I32);
                }
                _ => todo!(),
            },
        }
    }

    fn free_stored_record(&mut self, types: impl IntoIterator<Item = Type>, value: u32) {
        let mut load_offset = 0;
        let mut store_offset = 0;
        for ty in types {
            let field_value = self.push_local(CoreType::I32);

            let abi = self.abi(ty);
            align(&mut load_offset, abi.align);

            self.push(Ins::LocalGet(value));
            self.push(Ins::I32Const(load_offset));
            self.push(Ins::I32Add);
            self.load(Ins::LocalSet(field_value));

            self.free_stored(ty, field_source);

            load_offset += abi.size;

            self.pop_local(field_value, CoreType::I32);
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
    ExportPostReturn,
    ExportLift,
    ExportLower,
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

    fn core_type(&self, resolve: &Resolve) -> (Vec<ValType>, Vec<ValType>) {
        match self.kind {
            FunctionKind::Import | FunctionKind::Export => (
                flatten_record_limit(
                    resolve,
                    self.params.iter().map(|(_, ty)| *ty),
                    MAX_FLAT_PARAMS,
                ),
                flatten_record_limit(resolve, self.results.iter(), MAX_FLAT_RESULTS),
            ),
            FunctionKind::ExportPostReturn => (
                flatten_record_limit(resolve, self.results.iter(), MAX_FLAT_RESULTS),
                Vec::new(),
            ),
            FunctionKind::ExportLift | FunctionKind::ExportLower => (
                vec![VecType::I32, VecType::I32, VecType::I32, VecType::I32],
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

    fn compile(&self, resolve: &Resolve) -> (Vec<ValType>, Vec<Ins>) {}
}

fn visit_function<'a>(
    functions: &mut Vec<MyFunction<'a>>,
    resolve: &'a Resolve,
    interface: Option<&'a str>,
    name: &'a str,
    params: &'a [(String, Type)],
    results: &'a Results,
    direction: Direction,
) {
    let make = |kind| MyFunction {
        kind,
        interface,
        name,
        params,
        results,
    };

    match direction {
        Direction::Import => {
            functions.push(make(FunctionKind::Import));
        }
        Direction::Export => {
            functions.push(make(FunctionKind::Export));
            functions.push(make(FunctionKind::ExportPostReturn));
            functions.push(make(FunctionKind::ExportLift));
            functions.push(make(FunctionKind::ExportLower));
        }
    }
}

fn visit_functions(
    functions: &mut Vec<MyFunction>,
    resolve: &Resolve,
    items: &IndexMap<String, WorldItem>,
    direction: Direction,
) -> Result<()> {
    for (item_name, item) in items {
        match item {
            WorldItem::Interface(interface) => {
                let interface = &resolve.interfaces[interface];
                for (func_name, func) in interface.functions {
                    self.visit_function(
                        functions,
                        resolve,
                        Some(&interface.name),
                        func_name,
                        &func.params,
                        &func.results,
                        direction,
                    );
                }
            }

            WorldItem::Function(func) => {
                self.visit_func(
                    functions,
                    resolve,
                    None,
                    &func.name,
                    &func.params,
                    &func.results,
                    direction,
                );
            }

            WorldItem::Type(_) => bail!("type imports and exports not yet supported"),
        }
    }
    Ok(())
}

fn componentize(module: &[u8], resolve: &Resolve, world: WorldId) -> Result<Vec<u8>> {
    let mut my_functions = Vec::new();
    visit_functions(
        &mut my_functions,
        &resolve,
        &resolve.worlds[world].imports,
        Direction::Import,
    )?;
    visit_functions(
        &mut my_functions,
        &resolve,
        &resolve.worlds[world].exports,
        Direction::Export,
    )?;

    // First pass: find and count stuff
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
    let new_import_count = my_functions
        .iter()
        .filter(|f| matches!(f, FunctionKind::Import(_)))
        .count();
    let dispatchable_function_count = my_functions.iter().filter(|f| f.is_dispatchable()).count();
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

    let mut export_set = EXPORTS.iter().copied().collect::<HashSet<_>>();
    let mut export_map = HashMap::new();

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
                for function in &my_functions {
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
                for (index, function) in my_functions.iter().enumerate() {
                    if let FunctionKind::Import = function.kind {
                        imports.import(
                            function.interface.unwrap_or(&resolve.worlds[world].name),
                            function.name,
                            EntityType::Function(type_count + index),
                        );
                    }
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
                for index in 0..my_functions.len() {
                    functions.function(old_type_count + index);
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
                        if export_set.remove(name) {
                            if let ExternalKind::Func = export.kind {
                                export_map.insert(name, remap(export.index));
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
                for (index, function) in my_functions.enumerate() {
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
                    for function in &my_functions {
                        let (locals, instructions) =
                            function.compile(resolve, stack_pointer_index, &export_map);

                        let func = Function::new_with_locals_types(locals);
                        for instruction in &instructions {
                            func.instruction(instruction);
                        }
                        code_section.function(&func);
                    }

                    let dispatch = Function::new([]);

                    dispatch.instruction(&Ins::GlobalGet(table_count));
                    dispatch.instruction(&Ins::If(BlockType::Empty));
                    dispatch.instruction(&Ins::I32Const(0));
                    dispatch.instruction(&Ins::GlobalSet(table_count));

                    let table_index = 0;
                    for (index, function) in my_functions.iter().enumarate() {
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

                for (index, function) in my_functions
                    .iter()
                    .filter(|f| matches!(f.kind, FunctionKind::Import(_)))
                    .enumerate()
                {
                    function_names.push((
                        old_import_count - 1 + index,
                        format!("{}-import", function.internal_name()),
                    ));
                }

                for (index, function) in my_functions.iter().enumerate() {
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

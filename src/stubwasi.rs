use std::collections::HashMap;

use anyhow::{bail, Error};
use wasm_convert::IntoValType;
use wasm_encoder::{
    CodeSection, ExportKind, ExportSection, Function, FunctionSection, Instruction as Ins, Module,
    TypeSection,
};
use wasmparser::{FuncType, Parser, Payload, TypeRef};

use crate::Library;

type LinkedStubModules = Option<(Vec<u8>, Box<dyn Fn(u32) -> u32>)>;

pub fn link_stub_modules(libraries: Vec<Library>) -> Result<LinkedStubModules, Error> {
    let mut wasi_imports = HashMap::new();
    let mut linker = wit_component::Linker::default()
        .validate(true)
        .use_built_in_libdl(true);

    for Library {
        name,
        module,
        dl_openable,
    } in &libraries
    {
        add_wasi_imports(module, &mut wasi_imports)?;
        linker = linker.library(name, module, *dl_openable)?;
    }

    for (module, imports) in &wasi_imports {
        linker = linker.adapter(module, &make_stub_adapter(module, imports))?;
    }

    let component = linker.encode()?;

    // As of this writing, `wit_component::Linker` generates a component such that the first module is the
    // `main` one, followed by any adapters, followed by any libraries, followed by the `init` module, which is
    // finally followed by any shim modules.  Given that the stubbed component may contain more adapters than
    // the non-stubbed version, we need to tell `component-init` how to translate module indexes from the
    // former to the latter.
    //
    // TODO: this is pretty fragile in that it could silently break if `wit_component::Linker`'s implementation
    // changes.  Can we make it more robust?

    let old_adapter_count = 1;
    let new_adapter_count = u32::try_from(wasi_imports.len())?;
    assert!(new_adapter_count >= old_adapter_count);

    Ok(Some((
        component,
        Box::new(move |index: u32| {
            if index == 0 {
                // `main` module
                0
            } else if index <= new_adapter_count {
                // adapter module
                old_adapter_count
            } else {
                // one of the other kinds of module
                index + old_adapter_count - new_adapter_count
            }
        }),
    )))
}

fn add_wasi_imports<'a>(
    module: &'a [u8],
    imports: &mut HashMap<&'a str, HashMap<&'a str, FuncType>>,
) -> Result<(), Error> {
    let mut types = Vec::new();
    for payload in Parser::new(0).parse_all(module) {
        match payload? {
            Payload::TypeSection(reader) => {
                types = reader
                    .into_iter_err_on_gc_types()
                    .collect::<Result<Vec<_>, _>>()?;
            }

            Payload::ImportSection(reader) => {
                for import in reader {
                    let import = import?;

                    if import.module == "wasi_snapshot_preview1"
                        || import.module.starts_with("wasi:")
                    {
                        if let TypeRef::Func(ty) = import.ty {
                            imports
                                .entry(import.module)
                                .or_default()
                                .insert(import.name, types[usize::try_from(ty).unwrap()].clone());
                        } else {
                            bail!("encountered non-function import from WASI namespace")
                        }
                    }
                }
                break;
            }

            _ => {}
        }
    }

    Ok(())
}

fn make_stub_adapter(_module: &str, stubs: &HashMap<&str, FuncType>) -> Vec<u8> {
    let mut types = TypeSection::new();
    let mut functions = FunctionSection::new();
    let mut exports = ExportSection::new();
    let mut code = CodeSection::new();

    for (index, (name, ty)) in stubs.iter().enumerate() {
        let index = u32::try_from(index).unwrap();
        types.ty().function(
            ty.params().iter().map(|&v| IntoValType(v).into()),
            ty.results().iter().map(|&v| IntoValType(v).into()),
        );
        functions.function(index);
        exports.export(name, ExportKind::Func, index);
        let mut function = Function::new([]);
        function.instruction(&Ins::Unreachable);
        function.instruction(&Ins::End);
        code.function(&function);
    }

    let mut module = Module::new();
    module.section(&types);
    module.section(&functions);
    module.section(&exports);
    module.section(&code);

    module.finish()
}

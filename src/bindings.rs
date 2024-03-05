use {
    crate::{
        bindgen::{
            FunctionBindgen, DISPATCHABLE_CORE_PARAM_COUNT, DISPATCH_CORE_PARAM_COUNT, IMPORTS,
            IMPORT_SIGNATURES,
        },
        summary::{FunctionKind, Summary},
    },
    anyhow::Result,
    indexmap::IndexSet,
    std::borrow::Cow,
    wasm_encoder::{
        CodeSection, ConstExpr, CustomSection, ElementSection, Elements, Encode, EntityType,
        ExportKind, ExportSection, Function, FunctionSection, GlobalType, HeapType, ImportSection,
        Instruction as Ins, MemoryType, Module, RefType, TableType, TypeSection, ValType,
    },
    wit_component::metadata,
    wit_parser::{Resolve, WorldId},
};

const WASM_DYLINK_MEM_INFO: u8 = 1;
const WASM_DYLINK_NEEDED: u8 = 2;

struct MemInfo {
    memory_size: u32,
    memory_alignment: u32,
    table_size: u32,
    table_alignment: u32,
}

pub fn make_bindings(
    resolve: &Resolve,
    worlds: &IndexSet<WorldId>,
    summary: &Summary,
) -> Result<Vec<u8>> {
    // TODO: deduplicate types
    let mut types = TypeSection::new();
    let mut imports = ImportSection::new();
    let mut functions = FunctionSection::new();
    let mut exports = ExportSection::new();
    let mut code = CodeSection::new();
    let mut function_names = Vec::new();
    let mut global_names = Vec::new();

    for (name, params, results) in IMPORT_SIGNATURES {
        let offset = types.len();
        types.function(params.iter().copied(), results.iter().copied());
        imports.import("env", name, EntityType::Function(offset));
        function_names.push((offset, (*name).to_owned()));
    }

    for function in summary.functions.iter().filter(|f| {
        matches!(
            f.kind,
            FunctionKind::Import
                | FunctionKind::ResourceNew
                | FunctionKind::ResourceRep
                | FunctionKind::ResourceDropLocal
                | FunctionKind::ResourceDropRemote
        )
    }) {
        let module = &function
            .interface
            .as_ref()
            .map(|interface| {
                format!(
                    "{}{}",
                    if matches!(
                        function.kind,
                        FunctionKind::Import | FunctionKind::ResourceDropRemote
                    ) {
                        ""
                    } else {
                        "[export]"
                    },
                    if let Some(name) = resolve.id_of(interface.id) {
                        name
                    } else {
                        interface.name.to_owned()
                    }
                )
            })
            .unwrap_or_else(|| "$root".to_owned());

        let name = function.name;
        let name = &match function.kind {
            FunctionKind::ResourceNew => format!("[resource-new]{name}"),
            FunctionKind::ResourceRep => format!("[resource-rep]{name}"),
            FunctionKind::ResourceDropLocal | FunctionKind::ResourceDropRemote => {
                format!("[resource-drop]{name}")
            }
            _ => name.to_owned(),
        };

        let (params, results) = function.core_import_type(resolve);
        let offset = types.len();

        types.function(params, results);
        imports.import(module, name, EntityType::Function(offset));
        function_names.push((
            offset,
            format!("{}-imported", function.internal_name(resolve)),
        ));
    }

    let import_function_count = imports.len();

    let table_base = 0;
    imports.import(
        "env",
        "__table_base",
        EntityType::Global(GlobalType {
            val_type: ValType::I32,
            mutable: false,
        }),
    );
    global_names.push((table_base, "__table_base".to_owned()));

    let stack_pointer = 1;
    imports.import(
        "env",
        "__stack_pointer",
        EntityType::Global(GlobalType {
            val_type: ValType::I32,
            mutable: true,
        }),
    );
    global_names.push((stack_pointer, "__stack_pointer".to_owned()));

    imports.import(
        "env",
        "memory",
        EntityType::Memory(MemoryType {
            minimum: 0,
            maximum: None,
            memory64: false,
            shared: false,
        }),
    );

    imports.import(
        "env",
        "__indirect_function_table",
        EntityType::Table(TableType {
            element_type: RefType {
                nullable: true,
                heap_type: HeapType::Func,
            },
            minimum: summary
                .functions
                .iter()
                .filter(|function| function.is_dispatchable())
                .count()
                .try_into()
                .unwrap(),
            maximum: None,
        }),
    );

    let export_set = summary
        .functions
        .iter()
        .filter_map(|f| {
            if let FunctionKind::Export = f.kind {
                Some((f.interface.as_ref().map(|i| i.name), f.name))
            } else {
                None
            }
        })
        .collect::<IndexSet<_>>();

    let mut import_index = IMPORT_SIGNATURES.len();
    let mut dispatch_index = 0;
    for (index, function) in summary.functions.iter().enumerate() {
        let offset = types.len();
        let (params, results) = function.core_export_type(resolve);
        types.function(params, results);
        functions.function(offset);
        function_names.push((offset, function.internal_name(resolve)));
        let mut gen = FunctionBindgen::new(summary, function, stack_pointer);

        match function.kind {
            FunctionKind::Import => {
                gen.compile_import(import_index.try_into().unwrap());
                import_index += 1;
            }
            FunctionKind::Export => gen.compile_export(
                export_set
                    .get_index_of(&(function.interface.as_ref().map(|i| i.name), function.name))
                    .unwrap()
                    .try_into()?,
                // The next two `dispatch_index`es should be the from_canon and to_canon functions (see ordering in
                // `Summary::visit_function`):
                dispatch_index,
                dispatch_index + 1,
            ),
            FunctionKind::ExportFromCanon => gen.compile_export_from_canon(),
            FunctionKind::ExportToCanon => gen.compile_export_to_canon(),
            FunctionKind::ExportPostReturn => gen.compile_export_post_return(),
            FunctionKind::ResourceNew => {
                gen.compile_resource_new(import_index.try_into().unwrap());
                import_index += 1;
            }
            FunctionKind::ResourceRep => {
                gen.compile_resource_rep(import_index.try_into().unwrap());
                import_index += 1;
            }
            FunctionKind::ResourceDropLocal | FunctionKind::ResourceDropRemote => {
                gen.compile_resource_drop(import_index.try_into().unwrap());
                import_index += 1;
            }
        };

        let mut func = Function::new_with_locals_types(gen.local_types);
        for instruction in &gen.instructions {
            func.instruction(instruction);
        }
        func.instruction(&Ins::End);
        code.function(&func);

        if function.is_dispatchable() {
            dispatch_index += 1;
        }

        match function.kind {
            FunctionKind::Export | FunctionKind::ExportPostReturn => {
                exports.export(
                    &format!(
                        "{}{}",
                        if let FunctionKind::ExportPostReturn = function.kind {
                            "cabi_post_"
                        } else {
                            ""
                        },
                        if let Some(interface) = &function.interface {
                            format!(
                                "{}#{}",
                                if let Some(name) = resolve.id_of(interface.id) {
                                    name
                                } else {
                                    interface.name.to_owned()
                                },
                                function.name
                            )
                        } else {
                            function.name.to_owned()
                        }
                    ),
                    ExportKind::Func,
                    import_function_count + u32::try_from(index).unwrap(),
                );
            }

            _ => (),
        }
    }

    {
        let dispatch_offset = types.len();
        types.function([ValType::I32; DISPATCH_CORE_PARAM_COUNT], []);
        let dispatchable_offset = types.len();
        types.function([ValType::I32; DISPATCHABLE_CORE_PARAM_COUNT], []);
        functions.function(dispatch_offset);
        let name = "componentize-py#CallIndirect";
        function_names.push((dispatch_offset, name.to_owned()));
        let mut dispatch = Function::new([]);

        for local in 0..DISPATCH_CORE_PARAM_COUNT {
            dispatch.instruction(&Ins::LocalGet(u32::try_from(local).unwrap()));
        }
        dispatch.instruction(&Ins::GlobalGet(table_base));
        dispatch.instruction(&Ins::I32Add);
        dispatch.instruction(&Ins::CallIndirect {
            ty: dispatchable_offset,
            table: 0,
        });
        dispatch.instruction(&Ins::End);

        code.function(&dispatch);

        exports.export(name, ExportKind::Func, dispatch_offset);
    }

    exports.export(
        "cabi_import_realloc",
        ExportKind::Func,
        *IMPORTS.get("cabi_realloc").unwrap(),
    );

    exports.export(
        "cabi_export_realloc",
        ExportKind::Func,
        *IMPORTS.get("cabi_realloc").unwrap(),
    );

    let mut elements = ElementSection::new();
    elements.active(
        Some(0),
        &ConstExpr::global_get(table_base),
        Elements::Functions(
            &summary
                .functions
                .iter()
                .enumerate()
                .filter_map(|(index, function)| {
                    function
                        .is_dispatchable()
                        .then_some(import_function_count + u32::try_from(index).unwrap())
                })
                .collect::<Vec<_>>(),
        ),
    );

    let mut names_data = Vec::new();
    for (code, names) in [(0x01_u8, &function_names), (0x07_u8, &global_names)] {
        let mut subsection = Vec::new();
        names.len().encode(&mut subsection);
        for (index, name) in names {
            index.encode(&mut subsection);
            name.encode(&mut subsection);
        }
        names_data.push(code);
        subsection.encode(&mut names_data);
    }

    let mem_info = MemInfo {
        memory_size: 0,
        memory_alignment: 0,
        table_size: summary
            .functions
            .iter()
            .filter(|function| function.is_dispatchable())
            .count()
            .try_into()
            .unwrap(),
        table_alignment: 0,
    };

    let mut mem_info_subsection = Vec::new();
    mem_info.memory_size.encode(&mut mem_info_subsection);
    mem_info.memory_alignment.encode(&mut mem_info_subsection);
    mem_info.table_size.encode(&mut mem_info_subsection);
    mem_info.table_alignment.encode(&mut mem_info_subsection);

    let mut needed_subsection = Vec::new();
    1_u32.encode(&mut needed_subsection);
    "libcomponentize_py_runtime.so".encode(&mut needed_subsection);

    let mut dylink0 = Vec::new();
    dylink0.push(WASM_DYLINK_MEM_INFO);
    mem_info_subsection.encode(&mut dylink0);
    dylink0.push(WASM_DYLINK_NEEDED);
    needed_subsection.encode(&mut dylink0);

    let mut result = Module::new();
    result.section(&CustomSection {
        name: Cow::Borrowed("dylink.0"),
        data: Cow::Borrowed(&dylink0),
    });
    result.section(&types);
    result.section(&imports);
    result.section(&functions);
    result.section(&exports);
    result.section(&elements);
    result.section(&code);
    result.section(&CustomSection {
        name: Cow::Borrowed("name"),
        data: Cow::Borrowed(&names_data),
    });
    for &world in worlds {
        result.section(&CustomSection {
            name: Cow::Owned(format!("component-type:{}", resolve.worlds[world].name)),
            data: Cow::Owned(metadata::encode(
                resolve,
                world,
                wit_component::StringEncoding::UTF8,
                None,
            )?),
        });
    }

    let result = result.finish();

    wasmparser::validate(&result)?;

    Ok(result)
}

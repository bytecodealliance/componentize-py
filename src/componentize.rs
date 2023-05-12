use {
    crate::{
        bindgen::{self, FunctionBindgen, DISPATCH_CORE_PARAM_COUNT, LINK_LIST},
        convert::{
            self, IntoEntityType, IntoExportKind, IntoRefType, IntoTableType, IntoValType,
            MyElements,
        },
        summary::{FunctionKind, Summary},
    },
    anyhow::{bail, Result},
    indexmap::IndexSet,
    std::{cmp::Ordering, collections::HashMap, env, io::Cursor},
    wasm_encoder::{
        CodeSection, ConstExpr, CustomSection, ElementSection, Elements, Encode, EntityType,
        ExportKind, ExportSection, Function, FunctionSection, HeapType, ImportSection,
        Instruction as Ins, Module, RawSection, RefType, TableSection, TableType, TypeSection,
        ValType,
    },
    wasmparser::{ExternalKind, Name, NameSectionReader, Parser, Payload, TypeRef},
    wit_component::{metadata, ComponentEncoder},
    wit_parser::{Resolve, WorldId},
};

fn types_eq(
    wasmparser::Type::Func(a): &wasmparser::Type,
    wasmparser::Type::Func(b): &wasmparser::Type,
) -> bool {
    a == b
}

fn make_wasi_stub(name: &str) -> Vec<Ins> {
    // For most stubs, we trap, but we need specialized stubs for the functions called by `wasi-libc`'s
    // __wasm_call_ctors; otherwise we'd trap immediately upon calling any export.
    match name {
        "clock_time_get" => vec![
            // *time = 0;
            Ins::LocalGet(2),
            Ins::I64Const(0),
            Ins::I64Store(bindgen::mem_arg(0, 3)),
            // return ERRNO_SUCCESS;
            Ins::I32Const(0),
        ],
        "environ_sizes_get" => vec![
            // *environc = 0;
            Ins::LocalGet(0),
            Ins::I32Const(0),
            Ins::I32Store(bindgen::mem_arg(0, 2)),
            // *environ_buf_size = 0;
            Ins::LocalGet(1),
            Ins::I32Const(0),
            Ins::I32Store(bindgen::mem_arg(0, 2)),
            // return ERRNO_SUCCESS;
            Ins::I32Const(0),
        ],
        "fd_prestat_get" => vec![
            // return ERRNO_BADF;
            Ins::I32Const(8),
        ],
        _ => vec![Ins::Unreachable],
    }
}

pub fn componentize(
    module: &[u8],
    resolve: &Resolve,
    world: WorldId,
    summary: &Summary,
    stub_wasi: bool,
) -> Result<Vec<u8>> {
    // First pass: find stack pointer and `dispatch` function, and count various items

    let dispatch_type = wasmparser::Type::Func(wasmparser::FuncType::new(
        [wasmparser::ValType::I32; DISPATCH_CORE_PARAM_COUNT],
        [],
    ));
    let mut types = None;
    let mut import_count = None;
    let mut dispatch_import_index = None;
    let mut dispatch_type_index = None;
    let mut function_count = None;
    let mut table_count = None;
    let mut stack_pointer_index = None;
    let mut wasi_stubs = Vec::new();
    for payload in Parser::new(0).parse_all(module) {
        match payload? {
            Payload::TypeSection(reader) => {
                types = Some(reader.into_iter().collect::<Result<Vec<_>, _>>()?);
            }
            Payload::ImportSection(reader) => {
                let mut count = 0;
                for import in reader {
                    let import = import?;
                    match import.module {
                        "componentize-py" => {
                            if import.name == "dispatch" {
                                match import.ty {
                                    TypeRef::Func(ty)
                                        if types_eq(
                                            &types.as_ref().unwrap()[usize::try_from(ty).unwrap()],
                                            &dispatch_type,
                                        ) =>
                                    {
                                        dispatch_import_index = Some(count);
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
                                    import.name
                                );
                            }
                        }
                        "wasi_snapshot_preview1" => {
                            if let TypeRef::Func(ty) = import.ty {
                                if stub_wasi {
                                    wasi_stubs.push((ty, make_wasi_stub(import.name)));
                                }
                            } else {
                                bail!("unsupported WASI import type: {:?}", import.ty);
                            };
                        }
                        name => {
                            bail!("componentize-py import module has unrecognized name: {name}")
                        }
                    }
                    count += 1;
                }
                import_count = Some(count);
            }
            Payload::FunctionSection(reader) => {
                function_count = Some(reader.into_iter().count() + import_count.unwrap())
            }
            Payload::TableSection(reader) => {
                table_count = Some(reader.into_iter().count());
            }
            Payload::CustomSection(section) if section.name() == "name" => {
                let subsections = NameSectionReader::new(section.data(), section.data_offset());
                for subsection in subsections {
                    if let Name::Global(map) = subsection? {
                        for naming in map {
                            let naming = naming?;
                            if naming.name == "__stack_pointer" {
                                stack_pointer_index = Some(naming.index);
                                break;
                            }
                        }
                    }
                }
            }

            _ => {}
        }
    }

    // Second pass: generate a new module, removing the `componentize-py` imports and exports and adding the
    // imports, exports, generated functions, and dispatch table needed to implement the desired component type.

    let old_type_count = types.unwrap().len();
    let old_table_count = table_count.unwrap();
    let old_import_count = import_count.unwrap();
    let old_function_count = function_count.unwrap();
    let new_import_count = summary
        .functions
        .iter()
        .filter(|f| matches!(f.kind, FunctionKind::Import))
        .count();
    let dispatchable_function_count = summary
        .functions
        .iter()
        .filter(|f| f.is_dispatchable())
        .count();
    let dispatch_type_index = dispatch_type_index.unwrap();
    let dispatch_import_index = dispatch_import_index.unwrap();
    let stack_pointer_index = stack_pointer_index.unwrap();

    let remap = move |index: u32| match index.cmp(&dispatch_import_index.try_into().unwrap()) {
        Ordering::Less => index + u32::try_from(new_import_count).unwrap(),
        Ordering::Equal => (old_function_count + new_import_count - 1)
            .try_into()
            .unwrap(),
        Ordering::Greater => index + u32::try_from(new_import_count).unwrap() - 1,
    };

    let mut link_name_map = LINK_LIST
        .iter()
        .map(|&v| (format!("{v:?}"), v))
        .collect::<HashMap<_, _>>();

    let mut link_map = HashMap::new();

    let mut result = Module::new();
    let mut code_entries_remaining = old_function_count - old_import_count;
    let mut code_section = CodeSection::new();

    for payload in Parser::new(0).parse_all(module) {
        match payload? {
            Payload::TypeSection(reader) => {
                let mut types = TypeSection::new();
                for ty in reader {
                    let wasmparser::Type::Func(ty) = ty?;
                    let map = |&ty| IntoValType(ty).into();
                    types.function(ty.params().iter().map(map), ty.results().iter().map(map));
                }
                // TODO: should probably deduplicate these types:
                for function in summary
                    .functions
                    .iter()
                    .filter(|f| matches!(f.kind, FunctionKind::Import))
                {
                    let (params, results) = function.core_import_type(resolve);
                    types.function(params, results);
                }
                for function in &summary.functions {
                    let (params, results) = function.core_export_type(resolve);
                    types.function(params, results);
                }
                types.function([ValType::I32; 3], []);
                result.section(&types);
            }

            Payload::ImportSection(reader) => {
                let mut imports = ImportSection::new();
                for (index, function) in summary
                    .functions
                    .iter()
                    .filter(|f| matches!(f.kind, FunctionKind::Import))
                    .enumerate()
                {
                    imports.import(
                        function
                            .interface
                            .map(|i| i.name)
                            .unwrap_or(&resolve.worlds[world].name),
                        function.name,
                        EntityType::Function((old_type_count + index).try_into().unwrap()),
                    );
                }
                if !stub_wasi {
                    for import in reader
                        .into_iter()
                        .enumerate()
                        .filter_map(|(index, import)| {
                            (index != dispatch_import_index).then_some(import)
                        })
                    {
                        let import = import?;
                        imports.import(import.module, import.name, IntoEntityType(import.ty));
                    }
                }
                result.section(&imports);
            }

            Payload::FunctionSection(reader) => {
                let mut functions = FunctionSection::new();
                if stub_wasi {
                    for (ty, _) in &wasi_stubs {
                        functions.function(*ty);
                    }
                }
                for ty in reader {
                    functions.function(ty?);
                }
                // dispatch function:
                functions.function(dispatch_type_index);
                for index in 0..summary.functions.len() {
                    functions.function(
                        (old_type_count + new_import_count + index)
                            .try_into()
                            .unwrap(),
                    );
                }
                result.section(&functions);
            }

            Payload::TableSection(reader) => {
                let mut tables = TableSection::new();
                for table in reader {
                    let table = table?;
                    match table.init {
                        wasmparser::TableInit::RefNull => {
                            tables.table(IntoTableType(table.ty).into());
                        }
                        wasmparser::TableInit::Expr(expression) => {
                            tables.table_with_init(
                                IntoTableType(table.ty).into(),
                                &convert::const_expr(expression.get_binary_reader(), remap)?,
                            );
                        }
                    }
                }
                tables.table(TableType {
                    element_type: RefType {
                        nullable: true,
                        heap_type: HeapType::Func,
                    },
                    minimum: dispatchable_function_count.try_into().unwrap(),
                    maximum: Some(dispatchable_function_count.try_into().unwrap()),
                });
                result.section(&tables);
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
                            if let wasmparser::ExternalKind::Func = export.kind {
                                remap(export.index)
                            } else {
                                export.index
                            },
                        );
                    }
                }

                if !link_name_map.is_empty() {
                    bail!("missing expected exports: {:#?}", link_name_map.keys());
                }

                for (index, function) in summary.functions.iter().enumerate() {
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
                                    if let Some(interface) = function.interface {
                                        format!("{}#{}", interface.name, function.name)
                                    } else {
                                        function.name.to_owned()
                                    }
                                ),
                                ExportKind::Func,
                                (old_function_count + new_import_count + index)
                                    .try_into()
                                    .unwrap(),
                            );
                        }

                        _ => (),
                    }
                }
                result.section(&exports);
            }

            Payload::ElementSection(reader) => {
                let mut elements = ElementSection::new();
                for element in reader {
                    let element = element?;
                    match element.kind {
                        wasmparser::ElementKind::Passive => elements.passive(
                            IntoRefType(element.ty).into(),
                            MyElements::try_from((element.items, remap))?.as_elements(),
                        ),
                        wasmparser::ElementKind::Active {
                            table_index,
                            offset_expr,
                        } => elements.active(
                            Some(table_index),
                            &convert::const_expr(offset_expr.get_binary_reader(), remap)?,
                            IntoRefType(element.ty).into(),
                            MyElements::try_from((element.items, remap))?.as_elements(),
                        ),
                        wasmparser::ElementKind::Declared => elements.declared(
                            IntoRefType(element.ty).into(),
                            MyElements::try_from((element.items, remap))?.as_elements(),
                        ),
                    };
                }
                elements.active(
                    Some(old_table_count.try_into().unwrap()),
                    &ConstExpr::i32_const(0),
                    RefType {
                        nullable: true,
                        heap_type: HeapType::Func,
                    },
                    Elements::Functions(
                        &summary
                            .functions
                            .iter()
                            .enumerate()
                            .filter_map(|(index, function)| {
                                function.is_dispatchable().then_some(
                                    (old_function_count + new_import_count + index)
                                        .try_into()
                                        .unwrap(),
                                )
                            })
                            .collect::<Vec<_>>(),
                    ),
                );
                result.section(&elements);
            }

            Payload::CodeSectionStart { .. } => {
                if stub_wasi {
                    for (_, code) in &wasi_stubs {
                        let mut function = Function::new([]);
                        for ins in code {
                            function.instruction(ins);
                        }
                        function.instruction(&Ins::End);
                        code_section.function(&function);
                    }
                }
            }

            Payload::CodeSectionEntry(body) => {
                let mut reader = body.get_binary_reader();
                let mut locals = Vec::new();
                for _ in 0..reader.read_var_u32()? {
                    let count = reader.read_var_u32()?;
                    let ty = reader.read()?;
                    locals.push((count, IntoValType(ty).into()));
                }

                let mut function = Function::new(locals);
                function.raw(convert::visit(reader, remap)?);
                code_section.function(&function);

                code_entries_remaining = code_entries_remaining.checked_sub(1).unwrap();
                if code_entries_remaining == 0 {
                    let mut dispatch = Function::new([]);

                    let dispatch_param_count = 4;
                    for local in 0..dispatch_param_count {
                        dispatch.instruction(&Ins::LocalGet(local));
                    }
                    dispatch.instruction(&Ins::CallIndirect {
                        ty: (old_type_count + new_import_count + summary.functions.len())
                            .try_into()
                            .unwrap(),
                        table: old_table_count.try_into().unwrap(),
                    });
                    dispatch.instruction(&Ins::End);

                    code_section.function(&dispatch);

                    let exports = summary
                        .functions
                        .iter()
                        .filter_map(|f| {
                            if let FunctionKind::Export = f.kind {
                                Some((f.interface.map(|i| i.name), f.name))
                            } else {
                                None
                            }
                        })
                        .collect::<IndexSet<_>>();

                    let mut import_index = 0;
                    let mut dispatch_index = 0;
                    for function in &summary.functions {
                        let mut gen =
                            FunctionBindgen::new(summary, function, stack_pointer_index, &link_map);

                        match function.kind {
                            FunctionKind::Import => {
                                gen.compile_import(import_index.try_into().unwrap());
                                import_index += 1;
                            }
                            FunctionKind::Export => gen.compile_export(
                                exports
                                    .get_index_of(&(
                                        function.interface.map(|i| i.name),
                                        function.name,
                                    ))
                                    .unwrap()
                                    .try_into()?,
                                // next two `dispatch_index`es should be the lift and lower functions (see ordering
                                // in `Summary::visit_function`):
                                dispatch_index,
                                dispatch_index + 1,
                            ),
                            FunctionKind::ExportLift => gen.compile_export_lift(),
                            FunctionKind::ExportLower => gen.compile_export_lower(),
                            FunctionKind::ExportPostReturn => gen.compile_export_post_return(),
                        };

                        let mut func = Function::new_with_locals_types(gen.local_types);
                        for instruction in &gen.instructions {
                            func.instruction(instruction);
                        }
                        func.instruction(&Ins::End);
                        code_section.function(&func);

                        if function.is_dispatchable() {
                            dispatch_index += 1;
                        }
                    }

                    result.section(&code_section);
                }
            }

            Payload::CustomSection(section) if section.name() == "name" => {
                let mut function_names = Vec::new();
                let mut global_names = Vec::new();

                let subsections = NameSectionReader::new(section.data(), section.data_offset());
                for subsection in subsections {
                    match subsection? {
                        Name::Function(map) => {
                            for naming in map {
                                let naming = naming?;
                                function_names.push((remap(naming.index), naming.name.to_owned()));
                            }
                        }
                        Name::Global(map) => {
                            for naming in map {
                                let naming = naming?;
                                global_names.push((naming.index, naming.name.to_owned()));
                            }
                        }
                        // TODO: do we want to copy over other names as well?
                        _ => {}
                    }
                }

                function_names.push((
                    (old_function_count + new_import_count - 1)
                        .try_into()
                        .unwrap(),
                    "componentize-py#dispatch".to_owned(),
                ));

                for (index, function) in summary
                    .functions
                    .iter()
                    .filter(|f| matches!(f.kind, FunctionKind::Import))
                    .enumerate()
                {
                    function_names.push((
                        index.try_into().unwrap(),
                        format!("{}-import", function.internal_name()),
                    ));
                }

                for (index, function) in summary.functions.iter().enumerate() {
                    function_names.push((
                        (old_function_count + new_import_count + index)
                            .try_into()
                            .unwrap(),
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
                    data.push(code);
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
        name: &format!("component-type:{}", resolve.worlds[world].name),
        data: &metadata::encode(resolve, world, wit_component::StringEncoding::UTF8, None)?,
    });

    let result = result.finish();

    // Encode with WASI Preview 1 adapter
    ComponentEncoder::default()
        .validate(true)
        .module(&result)?
        .adapter(
            "wasi_snapshot_preview1",
            &zstd::decode_all(Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/wasi_snapshot_preview1.wasm.zst"
            ))))?,
        )?
        .encode()
}

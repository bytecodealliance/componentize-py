use {
    crate::{
        abi::{self, MAX_FLAT_PARAMS, MAX_FLAT_RESULTS},
        bindgen::DISPATCHABLE_CORE_PARAM_COUNT,
        util::Types as _,
    },
    anyhow::{bail, Result},
    componentize_py_shared::{self as symbols, Direction, Symbols},
    heck::{ToSnakeCase, ToUpperCamelCase},
    indexmap::{IndexMap, IndexSet},
    std::{collections::HashMap, fs::File, io::Write, iter, path::Path, str},
    wasm_encoder::ValType,
    wit_parser::{
        InterfaceId, Resolve, Results, Type, TypeDefKind, TypeId, TypeOwner, WorldId, WorldItem,
    },
};

pub enum FunctionKind {
    Import,
    Export,
    ExportLift,
    ExportLower,
    ExportPostReturn,
}

pub struct MyFunction<'a> {
    pub kind: FunctionKind,
    pub interface: Option<&'a str>,
    pub name: &'a str,
    pub params: &'a [(String, Type)],
    pub results: &'a Results,
}

impl<'a> MyFunction<'a> {
    pub fn internal_name(&self) -> String {
        if let Some(interface) = self.interface {
            format!(
                "{}#{}{}",
                interface,
                self.name,
                match self.kind {
                    FunctionKind::Import | FunctionKind::Export => "",
                    FunctionKind::ExportLift => "-lift",
                    FunctionKind::ExportLower => "-lower",
                    FunctionKind::ExportPostReturn => "-post-return",
                }
            )
        } else {
            self.name.to_owned()
        }
    }

    pub fn core_import_type(&self, resolve: &Resolve) -> (Vec<ValType>, Vec<ValType>) {
        let mut params =
            abi::record_abi_limit(resolve, self.params.types(), MAX_FLAT_PARAMS).flattened;

        let mut results = abi::record_abi(resolve, self.results.types()).flattened;

        if results.len() > MAX_FLAT_RESULTS {
            params.push(ValType::I32);
            results = Vec::new();
        };

        (params, results)
    }

    pub fn core_export_type(&self, resolve: &Resolve) -> (Vec<ValType>, Vec<ValType>) {
        match self.kind {
            FunctionKind::Export => (
                abi::record_abi_limit(resolve, self.params.types(), MAX_FLAT_PARAMS).flattened,
                abi::record_abi_limit(resolve, self.results.types(), MAX_FLAT_RESULTS).flattened,
            ),
            FunctionKind::Import | FunctionKind::ExportLift | FunctionKind::ExportLower => (
                vec![ValType::I32; DISPATCHABLE_CORE_PARAM_COUNT],
                Vec::new(),
            ),
            FunctionKind::ExportPostReturn => (vec![ValType::I32], Vec::new()),
        }
    }

    pub fn is_dispatchable(&self) -> bool {
        match self.kind {
            FunctionKind::Import | FunctionKind::ExportLift | FunctionKind::ExportLower => true,
            FunctionKind::Export | FunctionKind::ExportPostReturn => false,
        }
    }
}

pub struct Summary<'a> {
    pub resolve: &'a Resolve,
    pub functions: Vec<MyFunction<'a>>,
    pub types: IndexSet<TypeId>,
    pub imported_interfaces: HashMap<InterfaceId, &'a str>,
    pub exported_interfaces: HashMap<InterfaceId, &'a str>,
    pub tuple_types: HashMap<usize, TypeId>,
    pub option_type: Option<TypeId>,
    pub result_type: Option<TypeId>,
}

impl<'a> Summary<'a> {
    pub fn try_new(resolve: &'a Resolve, world: WorldId) -> Result<Self> {
        let mut me = Self {
            resolve,
            functions: Vec::new(),
            types: IndexSet::new(),
            exported_interfaces: HashMap::new(),
            imported_interfaces: HashMap::new(),
            tuple_types: HashMap::new(),
            option_type: None,
            result_type: None,
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
            | Type::String => (),
            Type::Id(id) => match &self.resolve.types[id].kind {
                TypeDefKind::Record(record) => {
                    self.types.insert(id);
                    for field in &record.fields {
                        self.visit_type(field.ty);
                    }
                }
                TypeDefKind::Variant(variant) => {
                    self.types.insert(id);
                    for case in &variant.cases {
                        if let Some(ty) = case.ty {
                            self.visit_type(ty);
                        }
                    }
                }
                TypeDefKind::Enum(_) | TypeDefKind::Flags(_) => {
                    self.types.insert(id);
                }
                TypeDefKind::Union(un) => {
                    self.types.insert(id);
                    for case in &un.cases {
                        self.visit_type(case.ty);
                    }
                }
                TypeDefKind::Option(ty) => {
                    if self.option_type.is_none() {
                        self.types.insert(id);
                        self.option_type = Some(id);
                    }
                    self.visit_type(*ty);
                }
                TypeDefKind::Result(result) => {
                    if self.result_type.is_none() {
                        self.types.insert(id);
                        self.result_type = Some(id);
                    }
                    if let Some(ty) = result.ok {
                        self.visit_type(ty);
                    }
                    if let Some(ty) = result.err {
                        self.visit_type(ty);
                    }
                }
                TypeDefKind::Tuple(tuple) => {
                    if !self.tuple_types.contains_key(&tuple.types.len()) {
                        self.types.insert(id);
                        self.tuple_types.insert(tuple.types.len(), id);
                    }
                    for ty in &tuple.types {
                        self.visit_type(*ty);
                    }
                }
                TypeDefKind::List(ty) => {
                    self.visit_type(*ty);
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
                if abi::record_abi(self.resolve, results.types())
                    .flattened
                    .len()
                    > MAX_FLAT_RESULTS
                {
                    self.functions.push(make(FunctionKind::ExportPostReturn));
                } else {
                    // As of this writing, no type involving heap allocation can fit into `MAX_FLAT_RESULTS`, so
                    // nothing to do.  We'll need to revisit this if `MAX_FLAT_RESULTS` changes or if new types are
                    // added.
                }
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
                        Direction::Import => self.imported_interfaces.insert(*interface, item_name),
                        Direction::Export => self.exported_interfaces.insert(*interface, item_name),
                    };
                    let interface = &self.resolve.interfaces[*interface];
                    for (func_name, func) in &interface.functions {
                        self.visit_function(
                            Some(item_name),
                            func_name,
                            &func.params,
                            &func.results,
                            direction,
                        );
                    }
                }

                WorldItem::Function(func) => {
                    self.visit_function(None, &func.name, &func.params, &func.results, direction);
                }

                WorldItem::Type(_) => bail!("type imports and exports not yet supported"),
            }
        }
        Ok(())
    }

    fn summarize_type(&self, ty: TypeId) -> symbols::Type<'a> {
        let ty = &self.resolve.types[ty];
        match ty.owner {
            TypeOwner::Interface(interface) => {
                let (direction, interface) =
                    if let Some(name) = self.imported_interfaces.get(&interface) {
                        (Direction::Import, *name)
                    } else {
                        (Direction::Export, self.exported_interfaces[&interface])
                    };

                symbols::Type::Owned(symbols::OwnedType {
                    direction,
                    interface,
                    name: ty.name.as_deref(),
                    fields: match &ty.kind {
                        TypeDefKind::Record(record) => {
                            record.fields.iter().map(|f| f.name.clone()).collect()
                        }
                        TypeDefKind::Variant(_) | TypeDefKind::Enum(_) | TypeDefKind::Union(_) => {
                            vec!["discriminant".to_owned(), "payload".to_owned()]
                        }
                        TypeDefKind::Flags(flags) => (0..flags.repr().count())
                            .map(|index| format!("word{index}"))
                            .collect(),
                        _ => todo!(),
                    },
                })
            }

            TypeOwner::None => match &ty.kind {
                TypeDefKind::Tuple(tuple) => symbols::Type::Tuple(tuple.types.len()),
                TypeDefKind::Option(_) | TypeDefKind::Result(_) => symbols::Type::Tuple(2),
                _ => todo!(),
            },

            TypeOwner::World(_) => todo!("handle types exported directly from a world: {ty:?}"),
        }
    }

    pub fn collect_symbols(&self) -> Symbols<'a> {
        let mut exports = Vec::new();
        for function in &self.functions {
            if let FunctionKind::Export = function.kind {
                exports.push(symbols::Function {
                    interface: function.interface,
                    name: function.name,
                });
            }
        }

        let mut types = Vec::new();
        for ty in &self.types {
            types.push(self.summarize_type(*ty));
        }

        Symbols { exports, types }
    }

    pub fn generate_code(&self, path: &Path) -> Result<()> {
        let mut interface_imports = HashMap::<_, Vec<_>>::new();
        let mut interface_exports = HashMap::<_, Vec<_>>::new();
        let mut world_imports = Vec::new();
        let mut index = 0;
        for function in &self.functions {
            #[allow(clippy::single_match)]
            match function.kind {
                FunctionKind::Import => {
                    // todo: generate typings
                    let snake = function.name.to_snake_case();

                    let params = function
                        .params
                        .iter()
                        .map(|(name, _)| name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");

                    let result_count = function.results.types().count();

                    let return_ = match result_count {
                        0 => "return",
                        1 => "return result[0]",
                        _ => "return result",
                    };

                    let code = format!(
                        r#"
def {snake}({params}):
    result = componentize_py.call_import({index}, [{params}], {result_count})
    {return_}

"#
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
            let ty = &self.resolve.types[*ty];
            match ty.owner {
                TypeOwner::Interface(interface) => {
                    // todo: reuse `wasmtime-py`'s code generation machinery

                    let make_class = |field_names: Vec<String>| {
                        let camel = if let Some(name) = &ty.name {
                            name.to_upper_camel_case()
                        } else {
                            format!("AnonymousType{index}")
                        };

                        let params = iter::once("self")
                            .chain(field_names.iter().map(String::as_str))
                            .collect::<Vec<_>>()
                            .join(", ");

                        let mut inits = field_names
                            .iter()
                            .map(|field_name| format!("self.{field_name} = {field_name}"))
                            .collect::<Vec<_>>()
                            .join("\n        ");

                        if inits.is_empty() {
                            inits = "pass".to_owned()
                        }

                        Some(format!(
                            r#"
class {camel}:
    def __init__({params}):
        {inits}

"#
                        ))
                    };

                    let code = match &ty.kind {
                        TypeDefKind::Record(record) => make_class(
                            record
                                .fields
                                .iter()
                                .map(|field| field.name.to_snake_case())
                                .collect::<Vec<_>>(),
                        ),
                        TypeDefKind::Variant(_)
                        | TypeDefKind::Enum(_)
                        | TypeDefKind::Union(_)
                        | TypeDefKind::Option(_)
                        | TypeDefKind::Result(_) => {
                            make_class(vec!["discriminant".to_owned(), "payload".to_owned()])
                        }
                        TypeDefKind::Flags(flags) => make_class(
                            (0..flags.repr().count())
                                .map(|index| format!("word{index}"))
                                .collect(),
                        ),
                        TypeDefKind::Tuple(_) | TypeDefKind::List(_) => None,
                        _ => todo!(),
                    };

                    if let Some(code) = code {
                        if let Some(name) = self.imported_interfaces.get(&interface) {
                            interface_imports.entry(name).or_default().push(code)
                        } else {
                            interface_exports
                                .entry(self.exported_interfaces[&interface])
                                .or_default()
                                .push(code)
                        }
                    }
                }

                TypeOwner::None => (),

                TypeOwner::World(_) => todo!("handle types exported directly from a world: {ty:?}"),
            }
        }

        for (name, code) in interface_imports {
            let mut file = File::create(path.join(&format!("{name}.py")))?;
            file.write_all(b"import componentize_py\n\n")?;
            for code in code {
                file.write_all(code.as_bytes())?;
            }
        }

        for (name, code) in interface_exports {
            let mut file = File::create(path.join(&format!("{name}.py")))?;
            for code in code {
                file.write_all(code.as_bytes())?;
            }
        }

        let mut file = File::create(path.join("__init__.py"))?;
        if !world_imports.is_empty() {
            file.write_all(b"import componentize_py\n\n")?;
            for code in world_imports {
                file.write_all(code.as_bytes())?;
            }
        }

        Ok(())
    }
}

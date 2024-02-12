use {
    crate::{
        abi::{self, MAX_FLAT_PARAMS, MAX_FLAT_RESULTS},
        bindgen::{self, DISPATCHABLE_CORE_PARAM_COUNT},
        exports::exports::{
            self, Case, Constructor, Function, FunctionExport, LocalResource, OwnedKind, OwnedType,
            RemoteResource, Resource, Static, Symbols,
        },
        util::Types as _,
    },
    anyhow::{bail, Result},
    heck::{ToShoutySnakeCase, ToSnakeCase, ToUpperCamelCase},
    indexmap::{IndexMap, IndexSet},
    once_cell::sync::Lazy,
    std::{
        collections::{hash_map::Entry, HashMap, HashSet},
        fmt::Write as _,
        fs::{self, File},
        io::Write as _,
        iter,
        ops::Deref,
        path::Path,
        str,
    },
    wasm_encoder::ValType,
    wit_parser::{
        Handle, InterfaceId, Resolve, Result_, Results, Type, TypeDefKind, TypeId, TypeOwner,
        WorldId, WorldItem, WorldKey,
    },
};

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Direction {
    Import,
    Export,
}

#[derive(Default, Copy, Clone)]
struct ResourceInfo {
    local_dispatch_index: Option<usize>,
    remote_dispatch_index: Option<usize>,
}

#[derive(Clone)]
struct ResourceState<'a> {
    direction: Direction,
    interface: Option<MyInterface<'a>>,
}

pub enum FunctionKind {
    Import,
    ResourceNew,
    ResourceRep,
    ResourceDropLocal,
    ResourceDropRemote,
    Export,
    ExportFromCanon,
    ExportToCanon,
    ExportPostReturn,
}

#[derive(Copy, Clone)]
pub struct PackageName<'a> {
    pub namespace: &'a str,
    pub name: &'a str,
}

#[derive(Clone)]
pub struct MyInterface<'a> {
    pub id: InterfaceId,
    pub package: Option<PackageName<'a>>,
    pub name: &'a str,
    pub docs: Option<&'a str>,
    pub resource_directions: im_rc::HashMap<TypeId, Direction>,
}

pub struct MyFunction<'a> {
    pub kind: FunctionKind,
    pub interface: Option<MyInterface<'a>>,
    pub name: &'a str,
    pub docs: Option<&'a str>,
    pub params: &'a [(String, Type)],
    pub results: &'a Results,
    pub wit_kind: wit_parser::FunctionKind,
}

impl<'a> MyFunction<'a> {
    pub fn internal_name(&self, resolve: &Resolve) -> String {
        if let Some(interface) = &self.interface {
            format!(
                "{}#{}{}",
                if let Some(name) = resolve.id_of(interface.id) {
                    name
                } else {
                    interface.name.to_owned()
                },
                self.name,
                match self.kind {
                    FunctionKind::Import => "-import",
                    FunctionKind::ResourceNew => "-resource-new",
                    FunctionKind::ResourceRep => "-resource-rep",
                    FunctionKind::ResourceDropLocal => "-resource-drop-local",
                    FunctionKind::ResourceDropRemote => "-resource-drop-remote",
                    FunctionKind::Export => "-export",
                    FunctionKind::ExportFromCanon => "-from-canon",
                    FunctionKind::ExportToCanon => "-to-canon",
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
            FunctionKind::Import
            | FunctionKind::ResourceNew
            | FunctionKind::ResourceRep
            | FunctionKind::ResourceDropLocal
            | FunctionKind::ResourceDropRemote
            | FunctionKind::ExportFromCanon
            | FunctionKind::ExportToCanon => (
                vec![ValType::I32; DISPATCHABLE_CORE_PARAM_COUNT],
                Vec::new(),
            ),
            FunctionKind::ExportPostReturn => (vec![ValType::I32], Vec::new()),
        }
    }

    pub fn is_dispatchable(&self) -> bool {
        match self.kind {
            FunctionKind::Import
            | FunctionKind::ResourceNew
            | FunctionKind::ResourceRep
            | FunctionKind::ResourceDropLocal
            | FunctionKind::ResourceDropRemote
            | FunctionKind::ExportFromCanon
            | FunctionKind::ExportToCanon => true,
            FunctionKind::Export | FunctionKind::ExportPostReturn => false,
        }
    }
}

pub struct InterfaceInfo<'a> {
    name: &'a str,
    docs: Option<&'a str>,
}

struct FunctionCode {
    snake: String,
    params: String,
    args: String,
    return_statement: String,
    class_method: &'static str,
    return_type: String,
    result_count: usize,
    error: Option<String>,
}

pub struct Summary<'a> {
    pub resolve: &'a Resolve,
    pub world: WorldId,
    pub functions: Vec<MyFunction<'a>>,
    pub types: IndexSet<TypeId>,
    pub imported_interfaces: HashMap<InterfaceId, InterfaceInfo<'a>>,
    pub exported_interfaces: HashMap<InterfaceId, InterfaceInfo<'a>>,
    pub tuple_types: HashMap<usize, TypeId>,
    pub option_type: Option<TypeId>,
    pub nesting_option_type: Option<TypeId>,
    pub result_type: Option<TypeId>,
    resource_state: Option<ResourceState<'a>>,
    resource_directions: im_rc::HashMap<TypeId, Direction>,
    resource_info: HashMap<TypeId, ResourceInfo>,
    dispatch_count: usize,
}

impl<'a> Summary<'a> {
    pub fn try_new(resolve: &'a Resolve, world: WorldId) -> Result<Self> {
        let mut me = Self {
            resolve,
            world,
            functions: Vec::new(),
            types: IndexSet::new(),
            exported_interfaces: HashMap::new(),
            imported_interfaces: HashMap::new(),
            tuple_types: HashMap::new(),
            option_type: None,
            nesting_option_type: None,
            result_type: None,
            resource_state: None,
            resource_directions: im_rc::HashMap::new(),
            resource_info: HashMap::new(),
            dispatch_count: 0,
        };

        me.visit_functions(&resolve.worlds[world].imports, Direction::Import)?;
        me.visit_functions(&resolve.worlds[world].exports, Direction::Export)?;

        me.types = me.types_sorted();

        Ok(me)
    }

    fn push_function(&mut self, function: MyFunction<'a>) {
        if function.is_dispatchable() {
            self.dispatch_count += 1;
        }
        self.functions.push(function);
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
            Type::Id(id) => {
                let ty = &self.resolve.types[id];
                match &ty.kind {
                    TypeDefKind::Record(record) => {
                        for field in &record.fields {
                            self.visit_type(field.ty);
                        }
                        self.types.insert(id);
                    }
                    TypeDefKind::Variant(variant) => {
                        for case in &variant.cases {
                            if let Some(ty) = case.ty {
                                self.visit_type(ty);
                            }
                        }
                        self.types.insert(id);
                    }
                    TypeDefKind::Enum(_) | TypeDefKind::Flags(_) | TypeDefKind::Handle(_) => {
                        self.types.insert(id);
                    }
                    TypeDefKind::Option(some) => {
                        if abi::is_option(self.resolve, *some) {
                            if self.nesting_option_type.is_none() {
                                self.nesting_option_type = Some(id);
                            }
                        } else if self.option_type.is_none() {
                            self.option_type = Some(id);
                        }
                        self.visit_type(*some);
                        self.types.insert(id);
                    }
                    TypeDefKind::Result(result) => {
                        if self.result_type.is_none() {
                            self.result_type = Some(id);
                        }
                        if let Some(ty) = result.ok {
                            self.visit_type(ty);
                        }
                        if let Some(ty) = result.err {
                            self.visit_type(ty);
                        }
                        self.types.insert(id);
                    }
                    TypeDefKind::Tuple(tuple) => {
                        if let Entry::Vacant(entry) = self.tuple_types.entry(tuple.types.len()) {
                            entry.insert(id);
                        }
                        for ty in &tuple.types {
                            self.visit_type(*ty);
                        }
                        self.types.insert(id);
                    }
                    TypeDefKind::List(ty) => {
                        self.visit_type(*ty);
                    }
                    TypeDefKind::Type(ty) => {
                        // When visiting a type alias, we must use the state already stored for any `use`d
                        // resources rather than overwrite it.
                        let resource_state = self.resource_state.take();
                        self.visit_type(*ty);
                        self.resource_state = resource_state;
                    }
                    TypeDefKind::Resource => {
                        if let Some(state) = self.resource_state.clone() {
                            self.resource_directions.insert(id, state.direction);
                            let info = self.resource_info.entry(id).or_default();

                            let make = |kind, params, results| MyFunction {
                                kind,
                                interface: state.interface.clone(),
                                name: ty.name.as_deref().unwrap(),
                                docs: None,
                                params,
                                results,
                                wit_kind: wit_parser::FunctionKind::Freestanding,
                            };

                            match state.direction {
                                Direction::Import => {
                                    info.remote_dispatch_index = Some(self.dispatch_count);

                                    static DROP_PARAMS: Lazy<[(String, Type); 1]> =
                                        Lazy::new(|| [("handle".to_string(), Type::U32)]);

                                    static DROP_RESULTS: Lazy<Results> = Lazy::new(Results::empty);

                                    self.push_function(make(
                                        FunctionKind::ResourceDropRemote,
                                        DROP_PARAMS.deref(),
                                        &DROP_RESULTS,
                                    ));
                                }

                                Direction::Export => {
                                    info.local_dispatch_index = Some(self.dispatch_count);

                                    // The order these functions are added must match the `LocalResource` field
                                    // initialization order in `summarize_type`.
                                    // TODO: make this less fragile.

                                    static NEW_PARAMS: Lazy<[(String, Type); 1]> =
                                        Lazy::new(|| [("rep".to_string(), Type::U32)]);

                                    static NEW_RESULTS: Results = Results::Anon(Type::U32);

                                    self.push_function(make(
                                        FunctionKind::ResourceNew,
                                        NEW_PARAMS.deref(),
                                        &NEW_RESULTS,
                                    ));

                                    static REP_PARAMS: Lazy<[(String, Type); 1]> =
                                        Lazy::new(|| [("handle".to_string(), Type::U32)]);

                                    static REP_RESULTS: Results = Results::Anon(Type::U32);

                                    self.push_function(make(
                                        FunctionKind::ResourceRep,
                                        REP_PARAMS.deref(),
                                        &REP_RESULTS,
                                    ));

                                    static DROP_PARAMS: Lazy<[(String, Type); 1]> =
                                        Lazy::new(|| [("handle".to_string(), Type::U32)]);

                                    static DROP_RESULTS: Lazy<Results> = Lazy::new(Results::empty);

                                    self.push_function(make(
                                        FunctionKind::ResourceDropLocal,
                                        DROP_PARAMS.deref(),
                                        DROP_RESULTS.deref(),
                                    ));
                                }
                            }
                        }
                        self.types.insert(id);
                    }
                    kind => todo!("{kind:?}"),
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn visit_function(
        &mut self,
        interface: Option<MyInterface<'a>>,
        name: &'a str,
        docs: Option<&'a str>,
        params: &'a [(String, Type)],
        results: &'a Results,
        direction: Direction,
        wit_kind: wit_parser::FunctionKind,
    ) {
        for ty in params.types() {
            self.visit_type(ty);
        }

        for ty in results.types() {
            self.visit_type(ty);
        }

        let make = |kind| MyFunction {
            kind,
            interface: interface.clone(),
            name,
            docs,
            params,
            results,
            wit_kind: wit_kind.clone(),
        };

        match direction {
            Direction::Import => {
                self.push_function(make(FunctionKind::Import));
            }
            Direction::Export => {
                // NB: We rely on this order when compiling, so please don't change it:
                // todo: make this less fragile
                self.push_function(make(FunctionKind::Export));
                self.push_function(make(FunctionKind::ExportFromCanon));
                self.push_function(make(FunctionKind::ExportToCanon));
                if abi::record_abi(self.resolve, results.types())
                    .flattened
                    .len()
                    > MAX_FLAT_RESULTS
                {
                    self.push_function(make(FunctionKind::ExportPostReturn));
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
        items: &'a IndexMap<WorldKey, WorldItem>,
        direction: Direction,
    ) -> Result<()> {
        for (key, item) in items {
            match item {
                WorldItem::Interface(id) => {
                    let (package, item_name) = match key {
                        wit_parser::WorldKey::Name(name) => (None, name),
                        wit_parser::WorldKey::Interface(id) => {
                            let interface = &self.resolve.interfaces[*id];
                            match &interface.name {
                                Some(name) => {
                                    if let Some(package) = interface.package {
                                        let package_name = &self.resolve.packages[package].name;
                                        (
                                            Some(PackageName {
                                                namespace: &package_name.namespace,
                                                name: &package_name.name,
                                            }),
                                            name,
                                        )
                                    } else {
                                        (None, name)
                                    }
                                }
                                None => bail!("anonymous interfaces not yet supported"),
                            }
                        }
                    };

                    let interface = &self.resolve.interfaces[*id];
                    let info = InterfaceInfo {
                        name: item_name,
                        docs: interface.docs.contents.as_deref(),
                    };

                    self.resource_state = Some(ResourceState {
                        direction,
                        interface: Some(MyInterface {
                            package,
                            name: item_name,
                            id: *id,
                            docs: interface.docs.contents.as_deref(),
                            resource_directions: Default::default(),
                        }),
                    });
                    for id in interface.types.values() {
                        self.visit_type(Type::Id(*id));
                    }
                    self.resource_state = None;

                    match direction {
                        Direction::Import => self.imported_interfaces.insert(*id, info),
                        Direction::Export => self.exported_interfaces.insert(*id, info),
                    };
                    for (func_name, func) in &interface.functions {
                        self.visit_function(
                            Some(MyInterface {
                                package,
                                name: item_name,
                                id: *id,
                                docs: interface.docs.contents.as_deref(),
                                resource_directions: self.resource_directions.clone(),
                            }),
                            func_name,
                            func.docs.contents.as_deref(),
                            &func.params,
                            &func.results,
                            direction,
                            func.kind.clone(),
                        );
                    }
                }

                WorldItem::Function(func) => {
                    self.visit_function(
                        None,
                        &func.name,
                        func.docs.contents.as_deref(),
                        &func.params,
                        &func.results,
                        direction,
                        func.kind.clone(),
                    );
                }

                WorldItem::Type(ty) => self.visit_type(Type::Id(*ty)),
            }
        }
        Ok(())
    }

    fn summarize_type(&self, id: TypeId, world_module: &str) -> exports::Type {
        let ty = &self.resolve.types[id];
        if let Some(package) = self.package(ty.owner, world_module) {
            let name = if let Some(name) = &ty.name {
                name.to_upper_camel_case().escape()
            } else {
                format!("AnonymousType{}", self.types.get_index_of(&id).unwrap())
            };
            let kind = match &ty.kind {
                TypeDefKind::Record(record) => OwnedKind::Record(
                    record
                        .fields
                        .iter()
                        .map(|f| f.name.to_snake_case().escape())
                        .collect(),
                ),
                TypeDefKind::Variant(variant) => OwnedKind::Variant(
                    variant
                        .cases
                        .iter()
                        .map(|c| Case {
                            name: format!("{name}{}", c.name.to_upper_camel_case().escape()),
                            has_payload: c.ty.is_some(),
                        })
                        .collect(),
                ),
                TypeDefKind::Enum(en) => OwnedKind::Enum(en.cases.len().try_into().unwrap()),
                TypeDefKind::Flags(flags) => {
                    OwnedKind::Flags(flags.repr().count().try_into().unwrap())
                }
                TypeDefKind::Tuple(_) | TypeDefKind::Option(_) | TypeDefKind::Result(_) => {
                    return self.summarize_unowned_type(id)
                }
                TypeDefKind::Resource => {
                    let info = &self.resource_info[&id];
                    OwnedKind::Resource(Resource {
                        local: info
                            .local_dispatch_index
                            .map(|dispatch_index| LocalResource {
                                // This must match the order the functions are added in `visit_type`:
                                new: u32::try_from(dispatch_index).unwrap(),
                                rep: u32::try_from(dispatch_index + 1).unwrap(),
                                drop: u32::try_from(dispatch_index + 2).unwrap(),
                            }),
                        remote: info
                            .remote_dispatch_index
                            .map(|dispatch_index| RemoteResource {
                                drop: u32::try_from(dispatch_index).unwrap(),
                            }),
                    })
                }
                kind => todo!("{kind:?}"),
            };

            exports::Type::Owned(OwnedType {
                package,
                name,
                kind,
            })
        } else {
            self.summarize_unowned_type(id)
        }
    }

    fn summarize_unowned_type(&self, id: TypeId) -> exports::Type {
        let ty = &self.resolve.types[id];
        match &ty.kind {
            TypeDefKind::Tuple(tuple) => {
                exports::Type::Tuple(tuple.types.len().try_into().unwrap())
            }
            TypeDefKind::Option(some) => {
                if abi::is_option(self.resolve, *some) {
                    exports::Type::NestingOption
                } else {
                    exports::Type::Option
                }
            }
            TypeDefKind::Result(_) => exports::Type::Result,
            TypeDefKind::Handle(_) => exports::Type::Handle,
            kind => todo!("{kind:?}"),
        }
    }

    pub fn collect_symbols(&self, world_module: &str) -> Symbols {
        let mut exports = Vec::new();
        for function in &self.functions {
            if let FunctionKind::Export = function.kind {
                let scope = if let Some(interface) = &function.interface {
                    interface.name
                } else {
                    world_module
                };

                exports.push(match function.wit_kind {
                    wit_parser::FunctionKind::Freestanding => {
                        FunctionExport::Freestanding(Function {
                            protocol: scope.to_upper_camel_case().escape(),
                            name: self.function_name(function),
                        })
                    }
                    wit_parser::FunctionKind::Constructor(id) => {
                        FunctionExport::Constructor(Constructor {
                            module: scope.to_snake_case().escape(),
                            protocol: self.resolve.types[id]
                                .name
                                .as_deref()
                                .unwrap()
                                .to_upper_camel_case()
                                .escape(),
                        })
                    }
                    wit_parser::FunctionKind::Method(_) => {
                        FunctionExport::Method(self.function_name(function))
                    }
                    wit_parser::FunctionKind::Static(id) => FunctionExport::Static(Static {
                        module: scope.to_snake_case().escape(),
                        protocol: self.resolve.types[id]
                            .name
                            .as_deref()
                            .unwrap()
                            .to_upper_camel_case()
                            .escape(),
                        name: self.function_name(function),
                    }),
                });
            }
        }

        let mut types = Vec::new();
        for ty in &self.types {
            types.push(self.summarize_type(*ty, world_module));
        }

        Symbols {
            types_package: format!("{world_module}.types"),
            exports,
            types,
        }
    }

    fn function_name(&self, function: &MyFunction) -> String {
        match function.wit_kind {
            wit_parser::FunctionKind::Freestanding => function.name.to_snake_case().escape(),
            wit_parser::FunctionKind::Constructor(_) => "__init__".into(),
            wit_parser::FunctionKind::Method(id) => function
                .name
                .strip_prefix(&format!(
                    "[method]{}.",
                    self.resolve.types[id].name.as_deref().unwrap()
                ))
                .unwrap()
                .to_snake_case()
                .escape(),
            wit_parser::FunctionKind::Static(id) => function
                .name
                .strip_prefix(&format!(
                    "[static]{}.",
                    self.resolve.types[id].name.as_deref().unwrap()
                ))
                .unwrap()
                .to_snake_case()
                .escape(),
        }
    }

    fn function_code(
        &self,
        direction: Direction,
        world_module: &str,
        function: &MyFunction,
        names: &mut TypeNames,
        seen: &HashSet<TypeId>,
        resource: Option<TypeId>,
    ) -> FunctionCode {
        enum SpecialReturn<'a> {
            Result(&'a Result_),
            None,
        }

        let special_return = |ty| {
            if let Type::Id(id) = ty {
                if let TypeDefKind::Result(result) = &self.resolve.types[id].kind {
                    SpecialReturn::Result(result)
                } else {
                    SpecialReturn::None
                }
            } else {
                SpecialReturn::None
            }
        };

        let snake = self.function_name(function);

        let (skip_count, self_) = match function.wit_kind {
            wit_parser::FunctionKind::Freestanding => (0, None),
            wit_parser::FunctionKind::Constructor(_) => (0, Some("self")),
            wit_parser::FunctionKind::Method(_) => (1, Some("self")),
            wit_parser::FunctionKind::Static(_) => (0, Some("cls")),
        };

        let mut type_name = |ty| names.type_name(ty, seen, resource);

        let absolute_type_name = |ty| {
            format!(
                "{world_module}.{}.{}",
                match direction {
                    Direction::Import => "imports",
                    Direction::Export => "exports",
                },
                TypeNames::new(self, TypeOwner::None).type_name(
                    ty,
                    &if let Type::Id(id) = ty {
                        Some(bindgen::dealias(self.resolve, id))
                    } else {
                        None
                    }
                    .into_iter()
                    .collect::<HashSet<_>>(),
                    None
                )
            )
        };

        let params = self_
            .map(|s| s.to_string())
            .into_iter()
            .chain(function.params.iter().skip(skip_count).map(|(name, ty)| {
                let snake = name.to_snake_case().escape();
                format!("{snake}: {}", type_name(*ty))
            }))
            .collect::<Vec<_>>()
            .join(", ");

        let args = function
            .params
            .iter()
            .map(|(name, _)| name.to_snake_case().escape())
            .collect::<Vec<_>>()
            .join(", ");

        let result_types = function.results.types().collect::<Vec<_>>();

        let (return_statement, return_type, error) =
            if let wit_parser::FunctionKind::Constructor(_) = function.wit_kind {
                ("return".to_owned(), "None".to_owned(), None)
            } else {
                let indent = if let wit_parser::FunctionKind::Freestanding = function.wit_kind {
                    ""
                } else {
                    "    "
                };

                match result_types.as_slice() {
                    [] => ("return".to_owned(), "None".to_owned(), None),
                    [ty] => match special_return(*ty) {
                        SpecialReturn::Result(result) => {
                            let error = if let Some(ty) = result.err {
                                Some(absolute_type_name(ty))
                            } else {
                                Some("None".into())
                            };

                            (
                                format!(
                                    "if isinstance(result[0], Err):
{indent}        raise result[0]
{indent}    else:
{indent}        return result[0].value"
                                ),
                                result.ok.map(type_name).unwrap_or_else(|| "None".into()),
                                error,
                            )
                        }
                        SpecialReturn::None => {
                            ("return result[0]".to_owned(), type_name(*ty), None)
                        }
                    },
                    _ => (
                        "return result".to_owned(),
                        format!(
                            "({})",
                            result_types
                                .iter()
                                .map(|ty| type_name(*ty))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        None,
                    ),
                }
            };

        let result_count = result_types.len();

        let class_method = if let wit_parser::FunctionKind::Static(_) = function.wit_kind {
            "\n    @classmethod"
        } else {
            ""
        };

        FunctionCode {
            snake,
            params,
            args,
            return_statement,
            class_method,
            return_type: format!(" -> {return_type}"),
            result_count,
            error,
        }
    }

    fn sort(&self, ty: Type, sorted: &mut IndexSet<TypeId>, visited: &mut HashSet<TypeId>) {
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
            Type::Id(id) => {
                let ty = &self.resolve.types[id];
                match &ty.kind {
                    TypeDefKind::Record(record) => {
                        for field in &record.fields {
                            self.sort(field.ty, sorted, visited);
                        }
                        sorted.insert(id);
                    }
                    TypeDefKind::Variant(variant) => {
                        for case in &variant.cases {
                            if let Some(ty) = case.ty {
                                self.sort(ty, sorted, visited);
                            }
                        }
                        sorted.insert(id);
                    }
                    TypeDefKind::Enum(_) | TypeDefKind::Flags(_) => {
                        sorted.insert(id);
                    }
                    TypeDefKind::Handle(Handle::Borrow(resource) | Handle::Own(resource)) => {
                        self.sort(Type::Id(*resource), sorted, visited);
                        sorted.insert(id);
                    }
                    TypeDefKind::Option(some) => {
                        self.sort(*some, sorted, visited);
                        sorted.insert(id);
                    }
                    TypeDefKind::Result(result) => {
                        if let Some(ty) = result.ok {
                            self.sort(ty, sorted, visited);
                        }
                        if let Some(ty) = result.err {
                            self.sort(ty, sorted, visited);
                        }
                        sorted.insert(id);
                    }
                    TypeDefKind::Tuple(tuple) => {
                        for ty in &tuple.types {
                            self.sort(*ty, sorted, visited);
                        }
                        sorted.insert(id);
                    }
                    TypeDefKind::List(ty) => {
                        self.sort(*ty, sorted, visited);
                    }
                    TypeDefKind::Type(ty) => {
                        self.sort(*ty, sorted, visited);
                    }
                    TypeDefKind::Resource => {
                        if !visited.contains(&id) {
                            visited.insert(id);

                            let sort = |function: &MyFunction, sorted: &mut _, visited: &mut _| {
                                for (_, ty) in function.params {
                                    self.sort(*ty, &mut *sorted, &mut *visited);
                                }

                                for ty in function.results.types() {
                                    self.sort(ty, &mut *sorted, &mut *visited);
                                }
                            };

                            let empty = &ResourceInfo::default();

                            if self
                                .resource_info
                                .get(&id)
                                .unwrap_or(empty)
                                .remote_dispatch_index
                                .is_some()
                            {
                                for function in &self.functions {
                                    if matches_resource(function, id, Direction::Import) {
                                        sort(function, sorted, visited);
                                    }
                                }
                            }

                            if self
                                .resource_info
                                .get(&id)
                                .unwrap_or(empty)
                                .local_dispatch_index
                                .is_some()
                            {
                                for function in &self.functions {
                                    if matches_resource(function, id, Direction::Export) {
                                        sort(function, sorted, visited);
                                    }
                                }
                            }

                            sorted.insert(id);
                        }
                    }
                    kind => todo!("{kind:?}"),
                }
            }
        }
    }

    fn types_sorted(&self) -> IndexSet<TypeId> {
        let mut sorted = IndexSet::new();
        let mut visited = HashSet::new();
        for id in &self.types {
            self.sort(Type::Id(*id), &mut sorted, &mut visited);
        }
        sorted
    }

    pub fn generate_code(
        &self,
        path: &Path,
        world_module: &str,
        stub_runtime_calls: bool,
    ) -> Result<()> {
        #[derive(Default)]
        struct Definitions<'a> {
            types: Vec<String>,
            functions: Vec<String>,
            type_imports: HashSet<InterfaceId>,
            function_imports: HashSet<InterfaceId>,
            docs: Option<&'a str>,
        }

        enum Code {
            None,
            Shared(String),
            Separate {
                import: Option<String>,
                export: Option<String>,
            },
        }

        let docstring = |docs: Option<&str>, indent_level, error: Option<&str>| {
            let docs = match (
                docs,
                error.map(|e| format!("Raises: `{world_module}.types.Err({e})`")),
            ) {
                (Some(docs), Some(error_docs)) => Some(format!("{docs}\n\n{error_docs}")),
                (Some(docs), None) => Some(docs.to_owned()),
                (None, Some(error_docs)) => Some(error_docs),
                (None, None) => None,
            };

            if let Some(docs) = docs {
                let newline = '\n';
                let indent = (0..indent_level)
                    .map(|_| "    ")
                    .collect::<Vec<_>>()
                    .concat();
                let docs = docs
                    .lines()
                    .map(|line| format!("{indent}{line}\n"))
                    .collect::<Vec<_>>()
                    .concat();
                format!(r#""""{newline}{docs}{indent}"""{newline}{indent}"#)
            } else {
                String::new()
            }
        };

        let mut interface_imports = HashMap::<&str, Definitions>::new();
        let mut interface_exports = HashMap::<&str, Definitions>::new();
        let mut world_imports = Definitions::default();
        let mut world_exports = Definitions::default();
        let mut seen = HashSet::new();
        for (index, id) in self.types.iter().copied().enumerate() {
            let ty = &self.resolve.types[id];
            let mut names = TypeNames::new(self, ty.owner);

            let camel = || {
                if let Some(name) = &ty.name {
                    name.to_upper_camel_case().escape()
                } else {
                    format!("AnonymousType{index}")
                }
            };

            let make_class = |names: &mut TypeNames, name, docs, fields: Vec<(String, Type)>| {
                let mut fields = fields
                    .iter()
                    .map(|(field_name, field_type)| {
                        format!(
                            "{field_name}: {}",
                            names.type_name(*field_type, &seen, None)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n    ");

                if fields.is_empty() {
                    fields = "pass".to_owned()
                }

                let docs = docstring(docs, 1, None);

                format!(
                    "
@dataclass
class {name}:
    {docs}{fields}
"
                )
            };

            let code = match &ty.kind {
                TypeDefKind::Record(record) => Code::Shared(make_class(
                    &mut names,
                    camel(),
                    ty.docs.contents.as_deref(),
                    record
                        .fields
                        .iter()
                        .map(|field| (field.name.to_snake_case().escape(), field.ty))
                        .collect::<Vec<_>>(),
                )),
                TypeDefKind::Variant(variant) => {
                    let camel = camel();
                    let classes = variant
                        .cases
                        .iter()
                        .map(|case| {
                            make_class(
                                &mut names,
                                format!("{camel}{}", case.name.to_upper_camel_case().escape()),
                                None,
                                if let Some(ty) = case.ty {
                                    vec![("value".into(), ty)]
                                } else {
                                    Vec::new()
                                },
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");

                    let cases = variant
                        .cases
                        .iter()
                        .map(|case| format!("{camel}{}", case.name.to_upper_camel_case().escape()))
                        .collect::<Vec<_>>()
                        .join(", ");

                    let docs = docstring(ty.docs.contents.as_deref(), 0, None);

                    Code::Shared(format!(
                        "
{classes}

{camel} = Union[{cases}]
{docs}
"
                    ))
                }
                TypeDefKind::Enum(en) => {
                    let camel = camel();
                    let cases = en
                        .cases
                        .iter()
                        .enumerate()
                        .map(|(index, case)| {
                            format!("{} = {index}", case.name.to_shouty_snake_case())
                        })
                        .collect::<Vec<_>>()
                        .join("\n    ");

                    let docs = docstring(ty.docs.contents.as_deref(), 1, None);

                    Code::Shared(format!(
                        "
class {camel}(Enum):
    {docs}{cases}
"
                    ))
                }
                TypeDefKind::Flags(flags) => {
                    let camel = camel();
                    let flags = flags
                        .flags
                        .iter()
                        .map(|flag| format!("{} = auto()", flag.name.to_shouty_snake_case()))
                        .collect::<Vec<_>>()
                        .join("\n    ");

                    let flags = if flags.is_empty() {
                        "pass".to_owned()
                    } else {
                        flags
                    };

                    let docs = docstring(ty.docs.contents.as_deref(), 1, None);

                    Code::Shared(format!(
                        "
class {camel}(Flag):
    {docs}{flags}
"
                    ))
                }
                TypeDefKind::Resource => {
                    let camel = camel();

                    let docs = docstring(ty.docs.contents.as_deref(), 1, None);

                    let empty = &ResourceInfo::default();

                    let import = if self
                        .resource_info
                        .get(&id)
                        .unwrap_or(empty)
                        .remote_dispatch_index
                        .is_some()
                    {
                        let method = |(index, function)| {
                            let FunctionCode {
                                snake,
                                params,
                                args,
                                return_type,
                                return_statement,
                                class_method,
                                result_count,
                                error,
                            } = self.function_code(
                                Direction::Import,
                                world_module,
                                function,
                                &mut names,
                                &seen,
                                Some(id),
                            );

                            let docs = docstring(function.docs, 2, error.as_deref());

                            if let wit_parser::FunctionKind::Constructor(_) = function.wit_kind {
                                if stub_runtime_calls {
                                    format!(
                                        "
    def {snake}({params}):
        {docs}raise NotImplementedError
"
                                    )
                                } else {
                                    format!(
                                        "
    def {snake}({params}):
        {docs}tmp = componentize_py_runtime.call_import({index}, [{args}], {result_count})[0]
        (_, func, args, _) = tmp.finalizer.detach()
        self.handle = tmp.handle
        self.finalizer = weakref.finalize(self, func, args[0], args[1])
"
                                    )
                                }
                            } else if stub_runtime_calls {
                                format!(
                                    "{class_method}
    def {snake}({params}){return_type}:
        {docs}raise NotImplementedError
"
                                )
                            } else {
                                format!(
                                    "{class_method}
    def {snake}({params}){return_type}:
        {docs}result = componentize_py_runtime.call_import({index}, [{args}], {result_count})
        {return_statement}
"
                                )
                            }
                        };

                        let methods = self
                            .functions
                            .iter()
                            .filter_map({
                                let mut index = 0;
                                move |function| {
                                    let result = matches_resource(function, id, Direction::Import)
                                        .then_some((index, function));

                                    if function.is_dispatchable() {
                                        index += 1;
                                    }

                                    result
                                }
                            })
                            .map(method)
                            .chain(iter::once({
                                let newline = '\n';
                                let indent = "        ";
                                let doc = "Release this resource.";
                                let docs =
                                    format!(r#""""{newline}{indent}{doc}{newline}{indent}"""{newline}{indent}"#);
                                let enter = r#"
    def __enter__(self):
        """Returns self"""
        return self
                                "#;
                                if stub_runtime_calls {
                                    format!(
                                        "{enter}                                    
    def __exit__(self, *args):
        {docs}raise NotImplementedError
"
                                    )
                                } else {
                                    format!(
                                        "{enter}
    def __exit__(self, *args):
        {docs}(_, func, args, _) = self.finalizer.detach()
        self.handle = None
        func(args[0], args[1])
"
                                    )
                                }
                            }))
                            .collect::<Vec<_>>()
                            .concat();

                        Some(format!(
                            "
class {camel}:
    {docs}{methods}
"
                        ))
                    } else {
                        None
                    };

                    let export = if self
                        .resource_info
                        .get(&id)
                        .unwrap_or(empty)
                        .local_dispatch_index
                        .is_some()
                    {
                        let method = |function| {
                            let FunctionCode {
                                snake,
                                params,
                                return_type,
                                class_method,
                                error,
                                ..
                            } = self.function_code(
                                Direction::Export,
                                world_module,
                                function,
                                &mut names,
                                &seen,
                                Some(id),
                            );

                            let docs = docstring(function.docs, 2, error.as_deref());

                            format!(
                                "{class_method}
    @abstractmethod
    def {snake}({params}){return_type}:
        {docs}raise NotImplementedError
"
                            )
                        };

                        let methods = self
                            .functions
                            .iter()
                            .filter(|function| matches_resource(function, id, Direction::Export))
                            .map(method)
                            .collect::<Vec<_>>()
                            .concat();

                        Some(format!(
                            "
class {camel}(Protocol):
    {docs}{methods}
"
                        ))
                    } else {
                        None
                    };

                    Code::Separate { import, export }
                }
                TypeDefKind::Tuple(_)
                | TypeDefKind::List(_)
                | TypeDefKind::Option(_)
                | TypeDefKind::Result(_)
                | TypeDefKind::Handle(_) => Code::None,
                kind => todo!("{kind:?}"),
            };

            let code = match code {
                Code::Shared(code) if self.has_imported_and_exported_resource(Type::Id(id)) => {
                    Code::Separate {
                        import: Some(code.clone()),
                        export: Some(code),
                    }
                }
                code => code,
            };

            match code {
                Code::None => {}
                Code::Shared(_) | Code::Separate { .. } => {
                    let tuples = match ty.owner {
                        TypeOwner::Interface(interface) => match code {
                            Code::None => unreachable!(),
                            Code::Shared(code) => vec![(
                                code,
                                if let Some(info) = self.imported_interfaces.get(&interface) {
                                    (interface_imports.entry(info.name).or_default(), info.docs)
                                } else if let Some(info) = self.exported_interfaces.get(&interface)
                                {
                                    (interface_exports.entry(info.name).or_default(), info.docs)
                                } else {
                                    unreachable!()
                                },
                            )],
                            Code::Separate { import, export } => import
                                .map(|code| {
                                    let info = self.imported_interfaces.get(&interface).unwrap();
                                    (
                                        code,
                                        (
                                            interface_imports.entry(info.name).or_default(),
                                            info.docs,
                                        ),
                                    )
                                })
                                .into_iter()
                                .chain(export.map(|code| {
                                    let info = self.exported_interfaces.get(&interface).unwrap();
                                    (
                                        code,
                                        (
                                            interface_exports.entry(info.name).or_default(),
                                            info.docs,
                                        ),
                                    )
                                }))
                                .collect(),
                        },

                        TypeOwner::World(_) => {
                            let docs = self.resolve.worlds[self.world].docs.contents.as_deref();
                            match code {
                                Code::None => unreachable!(),
                                Code::Shared(code) => vec![(code, (&mut world_exports, docs))],
                                Code::Separate { import, export } => import
                                    .map(|code| (code, (&mut world_imports, docs)))
                                    .into_iter()
                                    .chain(export.map(|code| (code, (&mut world_exports, docs))))
                                    .collect(),
                            }
                        }

                        TypeOwner::None => unreachable!(),
                    };

                    for (code, (definitions, docs)) in tuples {
                        definitions.types.push(code);
                        definitions.type_imports.extend(names.imports.clone());
                        definitions.docs = docs;
                    }
                }
            }

            seen.insert(id);
        }

        let mut index = 0;
        for function in &self.functions {
            #[allow(clippy::single_match)]
            match (&function.kind, &function.wit_kind) {
                (
                    FunctionKind::Import | FunctionKind::Export,
                    wit_parser::FunctionKind::Freestanding,
                ) => {
                    let mut names = TypeNames::new(
                        self,
                        if let FunctionKind::Export = function.kind {
                            TypeOwner::None
                        } else if let Some(interface) = &function.interface {
                            TypeOwner::Interface(interface.id)
                        } else {
                            TypeOwner::World(self.world)
                        },
                    );

                    let FunctionCode {
                        snake,
                        params,
                        args,
                        return_type,
                        return_statement,
                        result_count,
                        error,
                        ..
                    } = self.function_code(
                        Direction::Import,
                        world_module,
                        function,
                        &mut names,
                        &seen,
                        None,
                    );

                    match function.kind {
                        FunctionKind::Import => {
                            let docs = docstring(function.docs, 1, error.as_deref());

                            let code = if stub_runtime_calls {
                                format!(
                                    "
def {snake}({params}){return_type}:
    {docs}raise NotImplementedError
"
                                )
                            } else {
                                format!(
                                    "
def {snake}({params}){return_type}:
    {docs}result = componentize_py_runtime.call_import({index}, [{args}], {result_count})
    {return_statement}
"
                                )
                            };

                            let (definitions, docs) = if let Some(interface) = &function.interface {
                                (
                                    interface_imports.entry(interface.name).or_default(),
                                    interface.docs,
                                )
                            } else {
                                (
                                    &mut world_imports,
                                    self.resolve.worlds[self.world].docs.contents.as_deref(),
                                )
                            };

                            definitions.functions.push(code);
                            definitions.function_imports.extend(names.imports);
                            definitions.docs = docs;
                        }
                        FunctionKind::Export => {
                            let params = if params.is_empty() {
                                "self".to_owned()
                            } else {
                                format!("self, {params}")
                            };

                            let docs = docstring(function.docs, 2, error.as_deref());

                            let code = format!(
                                "
    @abstractmethod
    def {snake}({params}){return_type}:
        {docs}raise NotImplementedError
"
                            );

                            let (definitions, docs) = if let Some(interface) = &function.interface {
                                (
                                    interface_exports.entry(interface.name).or_default(),
                                    interface.docs,
                                )
                            } else {
                                (
                                    &mut world_exports,
                                    self.resolve.worlds[self.world].docs.contents.as_deref(),
                                )
                            };

                            definitions.functions.push(code);
                            definitions.function_imports.extend(names.imports);
                            definitions.docs = docs;
                        }
                        _ => unreachable!(),
                    }
                }
                _ => (),
            }

            if function.is_dispatchable() {
                index += 1;
            }
        }

        let python_imports =
            "from typing import TypeVar, Generic, Union, Optional, Union, Protocol, Tuple, List, Any, Self
from enum import Flag, Enum, auto
from dataclasses import dataclass
from abc import abstractmethod
import weakref
";

        {
            let mut file = File::create(path.join("types.py"))?;
            write!(
                file,
                "{python_imports}

S = TypeVar('S')
@dataclass
class Some(Generic[S]):
    value: S

T = TypeVar('T')
@dataclass
class Ok(Generic[T]):
    value: T

E = TypeVar('E')
@dataclass(frozen=True)
class Err(Generic[E], Exception):
    value: E

Result = Union[Ok[T], Err[E]]
            "
            )?;
        }

        let import = |prefix, interface| {
            let (module, package) = self.interface_package(interface);
            format!("from {prefix}{module} import {package}")
        };

        if !interface_imports.is_empty() {
            let dir = path.join("imports");
            fs::create_dir(&dir)?;
            File::create(dir.join("__init__.py"))?;
            for (name, code) in interface_imports {
                let mut file =
                    File::create(dir.join(&format!("{}.py", name.to_snake_case().escape())))?;
                let types = code.types.concat();
                let functions = code.functions.concat();
                let imports = code
                    .type_imports
                    .union(&code.function_imports)
                    .map(|&interface| import("..", interface))
                    .collect::<Vec<_>>()
                    .join("\n");
                let docs = docstring(code.docs, 0, None);

                let imports = if stub_runtime_calls {
                    imports
                } else {
                    format!("import componentize_py_runtime\n{imports}")
                };

                write!(
                    file,
                    "{docs}{python_imports}
from ..types import Result, Ok, Err, Some
{imports}
{types}
{functions}
"
                )?;
            }
        }

        if !interface_exports.is_empty() {
            let dir = path.join("exports");
            fs::create_dir(&dir)?;

            let mut protocol_imports = HashSet::new();
            let mut protocols = String::new();
            for (name, code) in interface_exports {
                let mut file =
                    File::create(dir.join(&format!("{}.py", name.to_snake_case().escape())))?;
                let types = code.types.concat();
                let imports = code
                    .type_imports
                    .into_iter()
                    .map(|interface| import("..", interface))
                    .collect::<Vec<_>>()
                    .join("\n");
                let docs = docstring(code.docs, 0, None);

                write!(
                    file,
                    "{docs}{python_imports}
from ..types import Result, Ok, Err, Some
{imports}
{types}
"
                )?;

                let camel = name.to_upper_camel_case().escape();
                let methods = if code.functions.is_empty() {
                    "    pass".to_owned()
                } else {
                    code.functions.concat()
                };

                protocol_imports.extend(code.function_imports);
                write!(
                    &mut protocols,
                    "
class {camel}(Protocol):
{methods}
"
                )?;
            }

            let mut init = File::create(dir.join("__init__.py"))?;
            let imports = protocol_imports
                .into_iter()
                .map(|interface| import("..", interface))
                .collect::<Vec<_>>()
                .join("\n");

            write!(
                init,
                "{python_imports}
from ..types import Result, Ok, Err, Some
{imports}
{protocols}
"
            )?;
        }

        {
            let mut file = File::create(path.join("__init__.py"))?;
            let function_imports = world_imports.functions.concat();
            let type_exports = world_exports.types.concat();
            let camel = self.resolve.worlds[self.world]
                .name
                .to_upper_camel_case()
                .escape();
            let methods = if world_exports.functions.is_empty() {
                "    pass".to_owned()
            } else {
                world_exports.functions.concat()
            };
            let imports = world_imports
                .function_imports
                .union(
                    &world_exports
                        .type_imports
                        .union(&world_exports.function_imports)
                        .copied()
                        .collect(),
                )
                .map(|&interface| import(".", interface))
                .collect::<Vec<_>>()
                .join("\n");
            let docs = docstring(world_exports.docs, 0, None);

            let imports = if stub_runtime_calls {
                imports
            } else {
                format!("import componentize_py_runtime\n{imports}")
            };

            write!(
                file,
                "{docs}{python_imports}
from .types import Result, Ok, Err, Some
{imports}
{function_imports}
{type_exports}
class {camel}(Protocol):
{methods}
"
            )?;
        }

        Ok(())
    }

    fn interface_package(&self, interface: InterfaceId) -> (&'static str, String) {
        if let Some(info) = self.imported_interfaces.get(&interface) {
            ("imports", info.name.to_snake_case().escape())
        } else {
            (
                "exports",
                self.exported_interfaces[&interface]
                    .name
                    .to_snake_case()
                    .escape(),
            )
        }
    }

    fn package(&self, owner: TypeOwner, world_module: &str) -> Option<String> {
        match owner {
            TypeOwner::Interface(interface) => {
                let (module, package) = self.interface_package(interface);
                Some(format!("{world_module}.{module}.{package}",))
            }
            TypeOwner::World(_) => Some(world_module.to_owned()),
            TypeOwner::None => None,
        }
    }

    fn has_imported_and_exported_resource(&self, ty: Type) -> bool {
        match ty {
            Type::Bool
            | Type::U8
            | Type::S8
            | Type::U16
            | Type::S16
            | Type::U32
            | Type::S32
            | Type::Char
            | Type::U64
            | Type::S64
            | Type::Float32
            | Type::Float64
            | Type::String => false,
            Type::Id(id) => match &self.resolve.types[id].kind {
                TypeDefKind::Record(record) => record
                    .fields
                    .iter()
                    .any(|field| self.has_imported_and_exported_resource(field.ty)),
                TypeDefKind::Variant(variant) => variant.cases.iter().any(|case| {
                    case.ty
                        .map(|ty| self.has_imported_and_exported_resource(ty))
                        .unwrap_or(false)
                }),
                TypeDefKind::Handle(Handle::Own(resource) | Handle::Borrow(resource)) => {
                    self.has_imported_and_exported_resource(Type::Id(*resource))
                }
                TypeDefKind::Enum(_) | TypeDefKind::Flags(_) => false,
                TypeDefKind::Result(result) => {
                    result
                        .ok
                        .map(|ty| self.has_imported_and_exported_resource(ty))
                        .unwrap_or(false)
                        || result
                            .err
                            .map(|ty| self.has_imported_and_exported_resource(ty))
                            .unwrap_or(false)
                }
                TypeDefKind::Tuple(tuple) => tuple
                    .types
                    .iter()
                    .any(|ty| self.has_imported_and_exported_resource(*ty)),
                TypeDefKind::Option(ty) | TypeDefKind::List(ty) | TypeDefKind::Type(ty) => {
                    self.has_imported_and_exported_resource(*ty)
                }
                TypeDefKind::Resource => {
                    let empty = &ResourceInfo::default();
                    let info = self.resource_info.get(&id).unwrap_or(empty);
                    info.local_dispatch_index.is_some() && info.remote_dispatch_index.is_some()
                }
                kind => todo!("{kind:?}"),
            },
        }
    }
}

struct TypeNames<'a> {
    summary: &'a Summary<'a>,
    owner: TypeOwner,
    imports: HashSet<InterfaceId>,
}

impl<'a> TypeNames<'a> {
    fn new(summary: &'a Summary<'_>, owner: TypeOwner) -> Self {
        Self {
            summary,
            owner,
            imports: HashSet::new(),
        }
    }

    fn type_name(&mut self, ty: Type, seen: &HashSet<TypeId>, resource: Option<TypeId>) -> String {
        match ty {
            Type::Bool
            | Type::U8
            | Type::U16
            | Type::U32
            | Type::U64
            | Type::S8
            | Type::S16
            | Type::S32
            | Type::S64 => "int".into(),
            Type::Float32 | Type::Float64 => "float".into(),
            Type::Char | Type::String => "str".into(),
            Type::Id(id) => {
                let ty = &self.summary.resolve.types[id];
                match &ty.kind {
                    TypeDefKind::Record(_)
                    | TypeDefKind::Variant(_)
                    | TypeDefKind::Enum(_)
                    | TypeDefKind::Flags(_)
                    | TypeDefKind::Resource => {
                        if seen.contains(&id) {
                            let package = if ty.owner == self.owner {
                                String::new()
                            } else {
                                match ty.owner {
                                    TypeOwner::Interface(interface) => {
                                        self.imports.insert(interface);
                                        format!("{}.", self.summary.interface_package(interface).1)
                                    }
                                    // todo: place anonymous types in types.py and import them from there
                                    _ => String::new(),
                                }
                            };

                            let name = if let Some(name) = &ty.name {
                                name.to_upper_camel_case().escape()
                            } else {
                                format!(
                                    "AnonymousType{}",
                                    self.summary.types.get_index_of(&id).unwrap()
                                )
                            };

                            format!("{package}{name}")
                        } else {
                            // As of this writing, there's no concept of forward declaration in Python, so we must
                            // either use `Any` or `Self` for types which have not yet been fully declared.
                            if Some(id) == resource { "Self" } else { "Any" }.to_owned()
                        }
                    }
                    TypeDefKind::Option(some) => {
                        if abi::is_option(self.summary.resolve, *some) {
                            format!("Optional[Some[{}]]", self.type_name(*some, seen, resource))
                        } else {
                            format!("Optional[{}]", self.type_name(*some, seen, resource))
                        }
                    }
                    TypeDefKind::Result(result) => format!(
                        "Result[{}, {}]",
                        result
                            .ok
                            .map(|ty| self.type_name(ty, seen, resource))
                            .unwrap_or_else(|| "None".into()),
                        result
                            .err
                            .map(|ty| self.type_name(ty, seen, resource))
                            .unwrap_or_else(|| "None".into())
                    ),
                    TypeDefKind::List(ty) => {
                        if let Type::U8 | Type::S8 = ty {
                            "bytes".into()
                        } else {
                            format!("List[{}]", self.type_name(*ty, seen, resource))
                        }
                    }
                    TypeDefKind::Tuple(tuple) => {
                        let types = tuple
                            .types
                            .iter()
                            .map(|ty| self.type_name(*ty, seen, resource))
                            .collect::<Vec<_>>()
                            .join(", ");
                        let types = if types.is_empty() {
                            "()".to_owned()
                        } else {
                            types
                        };
                        format!("Tuple[{types}]")
                    }
                    TypeDefKind::Handle(Handle::Own(ty) | Handle::Borrow(ty)) => {
                        self.type_name(Type::Id(*ty), seen, resource)
                    }
                    TypeDefKind::Type(ty) => self.type_name(*ty, seen, resource),
                    kind => todo!("{kind:?}"),
                }
            }
        }
    }
}

pub trait Escape {
    fn escape(self) -> Self;
}

impl Escape for String {
    fn escape(self) -> Self {
        // Escape Python keywords
        // Source: https://docs.python.org/3/reference/lexical_analysis.html#keywords
        match self.as_str() {
            "False" | "None" | "True" | "and" | "as" | "assert" | "async" | "await" | "break"
            | "class" | "continue" | "def" | "del" | "elif" | "else" | "except" | "finally"
            | "for" | "from" | "global" | "if" | "import" | "in" | "is" | "lambda" | "nonlocal"
            | "not" | "or" | "pass" | "raise" | "return" | "try" | "while" | "with" | "yield" => {
                format!("{self}_")
            }
            _ => self,
        }
    }
}

fn matches_resource(function: &MyFunction, resource: TypeId, direction: Direction) -> bool {
    match (direction, &function.kind) {
        (Direction::Import, FunctionKind::Import) | (Direction::Export, FunctionKind::Export) => {
            match &function.wit_kind {
                wit_parser::FunctionKind::Freestanding => false,
                wit_parser::FunctionKind::Method(id)
                | wit_parser::FunctionKind::Static(id)
                | wit_parser::FunctionKind::Constructor(id) => *id == resource,
            }
        }
        _ => false,
    }
}

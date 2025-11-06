use {
    crate::{
        exports::exports::{
            self, Case, Constructor, Function, FunctionExport, FunctionExportKind, ReturnStyle,
            Static, Symbols,
        },
        util::Types as _,
    },
    anyhow::{Result, bail},
    heck::{ToShoutySnakeCase, ToSnakeCase, ToUpperCamelCase},
    indexmap::{IndexMap, IndexSet},
    semver::Version,
    std::{
        collections::{HashMap, HashSet, hash_map::Entry},
        fmt::Write as _,
        fs::{self, File},
        io::Write as _,
        iter,
        path::Path,
        str,
    },
    wit_dylib::Metadata,
    wit_parser::{
        CloneMaps, Handle, InterfaceId, Resolve, Result_, Type, TypeDef, TypeDefKind, TypeId,
        TypeOwner, WorldId, WorldItem, WorldKey,
    },
};

const NOT_IMPLEMENTED: &str = "raise NotImplementedError";

const ASYNC_START_PREFIX: &str = "_async_start_";

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Direction {
    Import,
    Export,
}

#[derive(Default, Copy, Clone)]
struct ResourceInfo {
    local: bool,
    remote: bool,
}

#[derive(Clone)]
struct ResourceState {
    direction: Direction,
}

#[derive(Copy, Clone)]
pub enum FunctionKind {
    Import,
    Export,
}

#[derive(Copy, Clone)]
pub struct PackageName<'a> {
    pub namespace: &'a str,
    pub name: &'a str,
    pub version: Option<&'a Version>,
}

#[derive(Clone)]
pub struct MyInterface<'a> {
    pub id: InterfaceId,
    pub key: &'a WorldKey,
    pub docs: Option<&'a str>,
}

pub struct MyFunction<'a> {
    pub kind: FunctionKind,
    pub interface: Option<MyInterface<'a>>,
    pub name: &'a str,
    pub docs: Option<&'a str>,
    pub params: &'a [(String, Type)],
    pub result: &'a Option<Type>,
    pub wit_kind: wit_parser::FunctionKind,
}

impl MyFunction<'_> {
    fn key(&self) -> WorldKey {
        if let Some(interface) = self.interface.as_ref() {
            WorldKey::Interface(interface.id)
        } else {
            WorldKey::Name(self.name.into())
        }
    }
}

#[derive(Copy, Clone)]
pub struct InterfaceInfo<'a> {
    package: Option<PackageName<'a>>,
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
    error: Option<String>,
}

#[derive(Clone)]
enum Code {
    Shared(String),
    Separate {
        import: Option<String>,
        export: Option<String>,
    },
}

struct TypeLocation {
    module: String,
    aliases: Option<Code>,
}

#[derive(Default)]
pub struct Locations {
    types: HashMap<TypeId, TypeLocation>,
    keys: HashMap<WorldKey, String>,
}

pub struct Summary<'a> {
    pub resolve: &'a Resolve,
    pub functions: Vec<MyFunction<'a>>,
    pub types: IndexSet<TypeId>,
    pub imported_interfaces: HashMap<InterfaceId, InterfaceInfo<'a>>,
    pub exported_interfaces: HashMap<InterfaceId, InterfaceInfo<'a>>,
    pub tuple_types: HashMap<usize, TypeId>,
    pub option_type: Option<TypeId>,
    pub nesting_option_type: Option<TypeId>,
    pub result_type: Option<TypeId>,
    resource_state: Option<ResourceState>,
    resource_directions: im_rc::HashMap<TypeId, Direction>,
    resource_info: HashMap<TypeId, ResourceInfo>,
    world_types: HashMap<WorldId, HashSet<TypeId>>,
    world_keys: HashMap<WorldId, HashSet<(Direction, WorldKey)>>,
    imported_interface_names: HashMap<InterfaceId, String>,
    exported_interface_names: HashMap<InterfaceId, String>,
    imported_function_indexes: &'a HashMap<(Option<&'a str>, &'a str), usize>,
    exported_function_indexes: &'a HashMap<(Option<&'a str>, &'a str), usize>,
    stream_and_future_indexes: &'a HashMap<TypeId, usize>,
    need_async: bool,
}

impl<'a> Summary<'a> {
    pub fn try_new(
        resolve: &'a Resolve,
        worlds: &IndexSet<WorldId>,
        import_interface_names: &HashMap<&str, &str>,
        export_interface_names: &HashMap<&str, &str>,
        imported_function_indexes: &'a HashMap<(Option<&'a str>, &'a str), usize>,
        exported_function_indexes: &'a HashMap<(Option<&'a str>, &'a str), usize>,
        stream_and_future_indexes: &'a HashMap<TypeId, usize>,
    ) -> Result<Self> {
        let mut me = Self {
            resolve,
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
            world_types: HashMap::new(),
            world_keys: HashMap::new(),
            imported_interface_names: HashMap::new(),
            exported_interface_names: HashMap::new(),
            imported_function_indexes,
            exported_function_indexes,
            stream_and_future_indexes,
            need_async: false,
        };

        let mut import_keys_seen = HashSet::new();
        let mut export_keys_seen = HashSet::new();
        for &world in worlds {
            me.visit_functions(
                &resolve.worlds[world].imports,
                Direction::Import,
                world,
                &mut import_keys_seen,
            )?;
            me.visit_functions(
                &resolve.worlds[world].exports,
                Direction::Export,
                world,
                &mut export_keys_seen,
            )?;
        }

        me.types = me.types_sorted();

        me.imported_interface_names = me.interface_names(
            me.imported_interfaces.keys().copied(),
            import_interface_names,
        );
        me.exported_interface_names = me.interface_names(
            me.exported_interfaces.keys().copied(),
            export_interface_names,
        );

        Ok(me)
    }

    pub fn need_async(&self) -> bool {
        self.need_async
    }

    fn push_function(&mut self, function: MyFunction<'a>) {
        self.functions.push(function);
    }

    fn visit_type(&mut self, ty: Type, world: WorldId) {
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
            | Type::F32
            | Type::F64
            | Type::String
            | Type::ErrorContext => (),
            Type::Id(id) => {
                self.world_types.entry(world).or_default().insert(id);

                let ty = &self.resolve.types[id];
                match &ty.kind {
                    TypeDefKind::Record(record) => {
                        for field in &record.fields {
                            self.visit_type(field.ty, world);
                        }
                        self.types.insert(id);
                    }
                    TypeDefKind::Variant(variant) => {
                        for case in &variant.cases {
                            if let Some(ty) = case.ty {
                                self.visit_type(ty, world);
                            }
                        }
                        self.types.insert(id);
                    }
                    TypeDefKind::Enum(_) | TypeDefKind::Flags(_) | TypeDefKind::Handle(_) => {
                        self.types.insert(id);
                    }
                    TypeDefKind::Option(some) => {
                        if is_option(self.resolve, *some) {
                            if self.nesting_option_type.is_none() {
                                self.nesting_option_type = Some(id);
                            }
                        } else if self.option_type.is_none() {
                            self.option_type = Some(id);
                        }
                        self.visit_type(*some, world);
                        self.types.insert(id);
                    }
                    TypeDefKind::Result(result) => {
                        if self.result_type.is_none() {
                            self.result_type = Some(id);
                        }
                        if let Some(ty) = result.ok {
                            self.visit_type(ty, world);
                        }
                        if let Some(ty) = result.err {
                            self.visit_type(ty, world);
                        }
                        self.types.insert(id);
                    }
                    TypeDefKind::Tuple(tuple) => {
                        if let Entry::Vacant(entry) = self.tuple_types.entry(tuple.types.len()) {
                            entry.insert(id);
                        }
                        for ty in &tuple.types {
                            self.visit_type(*ty, world);
                        }
                        self.types.insert(id);
                    }
                    TypeDefKind::List(ty) => {
                        self.visit_type(*ty, world);
                    }
                    TypeDefKind::Type(ty) => {
                        // When visiting a type alias, we must use the state
                        // already stored for any `use`d resources rather than
                        // overwrite it.
                        let resource_state = self.resource_state.take();
                        self.visit_type(*ty, world);
                        self.resource_state = resource_state;
                    }
                    TypeDefKind::Resource => {
                        if let Some(state) = self.resource_state.clone() {
                            self.resource_directions.insert(id, state.direction);
                            let info = self.resource_info.entry(id).or_default();

                            match state.direction {
                                Direction::Import => {
                                    info.remote = true;
                                }

                                Direction::Export => {
                                    info.local = true;
                                }
                            }
                        }
                        self.types.insert(id);
                    }
                    TypeDefKind::Stream(ty) | TypeDefKind::Future(ty) => {
                        self.need_async = true;
                        if let Some(ty) = ty {
                            self.visit_type(*ty, world);
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
        result: &'a Option<Type>,
        direction: Direction,
        wit_kind: wit_parser::FunctionKind,
        world: WorldId,
    ) {
        for ty in params.types() {
            self.visit_type(ty, world);
        }

        for ty in result.types() {
            self.visit_type(ty, world);
        }

        let make = |kind| MyFunction {
            kind,
            interface: interface.clone(),
            name,
            docs,
            params,
            result,
            wit_kind: wit_kind.clone(),
        };

        if let wit_parser::FunctionKind::AsyncFreestanding
        | wit_parser::FunctionKind::AsyncMethod(_)
        | wit_parser::FunctionKind::AsyncStatic(_) = wit_kind
        {
            self.need_async = true;
        }

        match direction {
            Direction::Import => {
                self.push_function(make(FunctionKind::Import));
            }
            Direction::Export => {
                self.push_function(make(FunctionKind::Export));
            }
        }
    }

    fn visit_functions(
        &mut self,
        items: &'a IndexMap<WorldKey, WorldItem>,
        direction: Direction,
        world: WorldId,
        keys_seen: &mut HashSet<WorldKey>,
    ) -> Result<()> {
        for (key, item) in items {
            self.world_keys
                .entry(world)
                .or_default()
                .insert((direction, key.clone()));

            if keys_seen.contains(key) {
                continue;
            } else {
                keys_seen.insert(key.clone());
            }

            match item {
                WorldItem::Interface { id, .. } => {
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
                                                version: package_name.version.as_ref(),
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
                        package,
                        name: item_name,
                        docs: interface.docs.contents.as_deref(),
                    };

                    self.resource_state = Some(ResourceState { direction });
                    for id in interface.types.values() {
                        self.visit_type(Type::Id(*id), world);
                    }
                    self.resource_state = None;

                    match direction {
                        Direction::Import => self.imported_interfaces.insert(*id, info),
                        Direction::Export => self.exported_interfaces.insert(*id, info),
                    };
                    for (func_name, func) in &interface.functions {
                        self.visit_function(
                            Some(MyInterface {
                                id: *id,
                                key,
                                docs: interface.docs.contents.as_deref(),
                            }),
                            func_name,
                            func.docs.contents.as_deref(),
                            &func.params,
                            &func.result,
                            direction,
                            func.kind.clone(),
                            world,
                        );
                    }
                }

                WorldItem::Function(func) => {
                    self.visit_function(
                        None,
                        &func.name,
                        func.docs.contents.as_deref(),
                        &func.params,
                        &func.result,
                        direction,
                        func.kind.clone(),
                        world,
                    );
                }

                WorldItem::Type(ty) => self.visit_type(Type::Id(*ty), world),
            }
        }
        Ok(())
    }

    fn package_and_name(
        &self,
        id: TypeId,
        ty: &TypeDef,
        world_module: &str,
        reverse_cloned_interfaces: &HashMap<InterfaceId, InterfaceId>,
    ) -> Option<(String, String)> {
        if let Some(package) = self.package(ty.owner, world_module, reverse_cloned_interfaces) {
            let name = if let Some(name) = &ty.name {
                name.to_upper_camel_case().escape()
            } else {
                format!("AnonymousType{}", self.types.get_index_of(&id).unwrap())
            };

            Some((package, name))
        } else {
            None
        }
    }

    fn summarize_resource(
        &self,
        id: TypeId,
        world_module: &str,
        reverse_cloned_interfaces: &HashMap<InterfaceId, InterfaceId>,
    ) -> exports::Resource {
        let ty = &self.resolve.types[id];
        assert!(matches!(ty.kind, TypeDefKind::Resource));
        let (package, name) = self
            .package_and_name(id, ty, world_module, reverse_cloned_interfaces)
            .unwrap();

        exports::Resource { package, name }
    }

    fn summarize_record(
        &self,
        id: TypeId,
        world_module: &str,
        reverse_cloned_interfaces: &HashMap<InterfaceId, InterfaceId>,
    ) -> exports::Record {
        let ty = &self.resolve.types[id];
        let TypeDefKind::Record(record) = &ty.kind else {
            unreachable!()
        };
        let (package, name) = self
            .package_and_name(id, ty, world_module, reverse_cloned_interfaces)
            .unwrap();

        exports::Record {
            package,
            name,
            fields: record
                .fields
                .iter()
                .map(|f| f.name.to_snake_case().escape())
                .collect(),
        }
    }

    fn summarize_flags(
        &self,
        id: TypeId,
        world_module: &str,
        reverse_cloned_interfaces: &HashMap<InterfaceId, InterfaceId>,
    ) -> exports::Flags {
        let ty = &self.resolve.types[id];
        let TypeDefKind::Flags(flags) = &ty.kind else {
            unreachable!()
        };
        let (package, name) = self
            .package_and_name(id, ty, world_module, reverse_cloned_interfaces)
            .unwrap();

        exports::Flags {
            package,
            name,
            u32_count: flags.repr().count().try_into().unwrap(),
        }
    }

    fn summarize_variant(
        &self,
        id: TypeId,
        world_module: &str,
        reverse_cloned_interfaces: &HashMap<InterfaceId, InterfaceId>,
    ) -> exports::Variant {
        let ty = &self.resolve.types[id];
        let TypeDefKind::Variant(variant) = &ty.kind else {
            unreachable!()
        };
        let (package, name) = self
            .package_and_name(id, ty, world_module, reverse_cloned_interfaces)
            .unwrap();

        let cases = variant
            .cases
            .iter()
            .map(|c| Case {
                name: format!("{name}_{}", c.name.to_upper_camel_case().escape()),
                has_payload: c.ty.is_some(),
            })
            .collect();

        exports::Variant {
            package,
            name,
            cases,
        }
    }

    fn summarize_enum(
        &self,
        id: TypeId,
        world_module: &str,
        reverse_cloned_interfaces: &HashMap<InterfaceId, InterfaceId>,
    ) -> exports::Enum {
        let ty = &self.resolve.types[id];
        let TypeDefKind::Enum(enum_) = &ty.kind else {
            unreachable!()
        };
        let (package, name) = self
            .package_and_name(id, ty, world_module, reverse_cloned_interfaces)
            .unwrap();

        exports::Enum {
            package,
            name,
            count: enum_.cases.len().try_into().unwrap(),
        }
    }

    fn summarize_tuple(&self, id: TypeId) -> exports::Tuple {
        let TypeDefKind::Tuple(tuple) = &self.resolve.types[id].kind else {
            unreachable!()
        };

        exports::Tuple {
            count: tuple.types.len().try_into().unwrap(),
        }
    }

    fn summarize_option(&self, id: TypeId) -> exports::OptionKind {
        let TypeDefKind::Option(some) = &self.resolve.types[id].kind else {
            unreachable!()
        };

        if is_option(self.resolve, *some) {
            exports::OptionKind::Nesting
        } else {
            exports::OptionKind::NonNesting
        }
    }

    fn summarize_result(&self, id: TypeId) -> exports::ResultRecord {
        let TypeDefKind::Result(result) = &self.resolve.types[id].kind else {
            unreachable!()
        };

        exports::ResultRecord {
            has_ok: result.ok.is_some(),
            has_err: result.err.is_some(),
        }
    }

    pub fn collect_symbols(
        &self,
        locations: &Locations,
        metadata: &Metadata,
        clone_maps: &CloneMaps,
    ) -> Symbols {
        let mut map = HashMap::new();
        for function in &self.functions {
            if let FunctionKind::Export = function.kind {
                let scope = if let Some(interface) = &function.interface {
                    &self.exported_interface_names[&interface.id]
                } else {
                    locations.keys.get(&function.key()).unwrap()
                };

                map.insert(
                    (
                        function
                            .interface
                            .as_ref()
                            .map(|v| self.resolve.name_world_key(v.key)),
                        function.name,
                    ),
                    FunctionExport {
                        kind: match function.wit_kind {
                            wit_parser::FunctionKind::Freestanding
                            | wit_parser::FunctionKind::AsyncFreestanding => {
                                FunctionExportKind::Freestanding(Function {
                                    protocol: scope.to_upper_camel_case().escape(),
                                    name: self.function_name_for_call(function),
                                })
                            }
                            wit_parser::FunctionKind::Constructor(id) => {
                                FunctionExportKind::Constructor(Constructor {
                                    module: scope.to_snake_case().escape(),
                                    protocol: self.resolve.types[id]
                                        .name
                                        .as_deref()
                                        .unwrap()
                                        .to_upper_camel_case()
                                        .escape(),
                                })
                            }
                            wit_parser::FunctionKind::Method(_)
                            | wit_parser::FunctionKind::AsyncMethod(_) => {
                                FunctionExportKind::Method(self.function_name_for_call(function))
                            }
                            wit_parser::FunctionKind::Static(id)
                            | wit_parser::FunctionKind::AsyncStatic(id) => {
                                FunctionExportKind::Static(Static {
                                    module: scope.to_snake_case().escape(),
                                    protocol: self.resolve.types[id]
                                        .name
                                        .as_deref()
                                        .unwrap()
                                        .to_upper_camel_case()
                                        .escape(),
                                    name: self.function_name_for_call(function),
                                })
                            }
                        },
                        return_style: match function.result {
                            None => ReturnStyle::None,
                            &Some(Type::Id(id))
                                if matches!(
                                    &self.resolve.types[id].kind,
                                    TypeDefKind::Result(_)
                                ) =>
                            {
                                ReturnStyle::Result
                            }
                            _ => ReturnStyle::Normal,
                        },
                    },
                );
            }
        }

        let exports = metadata
            .export_funcs
            .iter()
            .map(|function| {
                map.remove(&(function.interface.clone(), &function.name))
                    .unwrap()
            })
            .collect();

        assert!(map.is_empty());

        let mut reverse_cloned_types = HashMap::new();
        for (&original, &clone) in clone_maps.types() {
            assert!(reverse_cloned_types.insert(clone, original).is_none());
        }

        let original = |ty| {
            if let Some(&original) = reverse_cloned_types.get(&ty) {
                original
            } else {
                ty
            }
        };

        let module = |ty| &locations.types.get(&ty).unwrap().module;

        let mut reverse_cloned_interfaces = HashMap::new();
        for (&original, &clone) in clone_maps.interfaces() {
            assert!(reverse_cloned_interfaces.insert(clone, original).is_none());
        }

        let resources = metadata
            .resources
            .iter()
            .map(|ty| original(ty.id))
            .map(|ty| self.summarize_resource(ty, module(ty), &reverse_cloned_interfaces))
            .collect();

        let records = metadata
            .records
            .iter()
            .map(|ty| original(ty.id))
            .map(|ty| self.summarize_record(ty, module(ty), &reverse_cloned_interfaces))
            .collect();

        let flags = metadata
            .flags
            .iter()
            .map(|ty| original(ty.id))
            .map(|ty| self.summarize_flags(ty, module(ty), &reverse_cloned_interfaces))
            .collect();

        let tuples = metadata
            .tuples
            .iter()
            .map(|ty| original(ty.id))
            .map(|ty| self.summarize_tuple(ty))
            .collect();

        let variants = metadata
            .variants
            .iter()
            .map(|ty| original(ty.id))
            .map(|ty| self.summarize_variant(ty, module(ty), &reverse_cloned_interfaces))
            .collect();

        let enums = metadata
            .enums
            .iter()
            .map(|ty| original(ty.id))
            .map(|ty| self.summarize_enum(ty, module(ty), &reverse_cloned_interfaces))
            .collect();

        let options = metadata
            .options
            .iter()
            .map(|ty| original(ty.id))
            .map(|ty| self.summarize_option(ty))
            .collect();

        let results = metadata
            .results
            .iter()
            .map(|ty| original(ty.id))
            .map(|ty| self.summarize_result(ty))
            .collect();

        Symbols {
            exports,
            resources,
            records,
            flags,
            tuples,
            variants,
            enums,
            options,
            results,
        }
    }

    fn imported_function_index(&self, function: &MyFunction) -> usize {
        *self
            .imported_function_indexes
            .get(&(
                function
                    .interface
                    .as_ref()
                    .map(|v| self.resolve.name_world_key(v.key))
                    .as_deref(),
                function.name,
            ))
            .unwrap()
    }

    fn exported_function_index(&self, function: &MyFunction) -> usize {
        *self
            .exported_function_indexes
            .get(&(
                function
                    .interface
                    .as_ref()
                    .map(|v| self.resolve.name_world_key(v.key))
                    .as_deref(),
                function.name,
            ))
            .unwrap()
    }

    fn function_name(&self, function: &MyFunction) -> String {
        self.function_name_with(&function.wit_kind, function.name, "")
    }

    fn function_name_for_call(&self, function: &MyFunction) -> String {
        self.function_name_with(&function.wit_kind, function.name, ASYNC_START_PREFIX)
    }

    fn function_name_with(
        &self,
        kind: &wit_parser::FunctionKind,
        name: &str,
        async_start_prefix: &str,
    ) -> String {
        match kind {
            wit_parser::FunctionKind::Freestanding => name.to_snake_case().escape(),
            wit_parser::FunctionKind::AsyncFreestanding => format!(
                "{async_start_prefix}{}",
                name.strip_prefix("[async]")
                    .unwrap()
                    .to_snake_case()
                    .escape()
            ),
            wit_parser::FunctionKind::Constructor(_) => "__init__".into(),
            wit_parser::FunctionKind::Method(id) => name
                .strip_prefix(&format!(
                    "[method]{}.",
                    self.resolve.types[*id].name.as_deref().unwrap()
                ))
                .unwrap()
                .to_snake_case()
                .escape(),
            wit_parser::FunctionKind::AsyncMethod(id) => format!(
                "{async_start_prefix}{}",
                name.strip_prefix(&format!(
                    "[async method]{}.",
                    self.resolve.types[*id].name.as_deref().unwrap()
                ))
                .unwrap()
                .to_snake_case()
                .escape()
            ),
            wit_parser::FunctionKind::Static(id) => name
                .strip_prefix(&format!(
                    "[static]{}.",
                    self.resolve.types[*id].name.as_deref().unwrap()
                ))
                .unwrap()
                .to_snake_case()
                .escape(),
            wit_parser::FunctionKind::AsyncStatic(id) => format!(
                "{async_start_prefix}{}",
                name.strip_prefix(&format!(
                    "[async static]{}.",
                    self.resolve.types[*id].name.as_deref().unwrap()
                ))
                .unwrap()
                .to_snake_case()
                .escape()
            ),
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
            wit_parser::FunctionKind::Freestanding
            | wit_parser::FunctionKind::AsyncFreestanding => (0, None),
            wit_parser::FunctionKind::Constructor(_) => (0, Some("self")),
            wit_parser::FunctionKind::Method(_) | wit_parser::FunctionKind::AsyncMethod(_) => {
                (1, Some("self"))
            }
            wit_parser::FunctionKind::Static(_) | wit_parser::FunctionKind::AsyncStatic(_) => {
                (0, Some("cls"))
            }
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
                        Some(dealias(self.resolve, id))
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

        let result_types = function.result.types().collect::<Vec<_>>();

        let (return_statement, return_type, error) =
            if let wit_parser::FunctionKind::Constructor(_) = function.wit_kind {
                ("return".to_owned(), "None".to_owned(), None)
            } else {
                let indent = if let wit_parser::FunctionKind::Freestanding
                | wit_parser::FunctionKind::AsyncFreestanding = function.wit_kind
                {
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
                                    "if isinstance(result, Err):
{indent}        raise result
{indent}    else:
{indent}        return result.value"
                                ),
                                result.ok.map(type_name).unwrap_or_else(|| "None".into()),
                                error,
                            )
                        }
                        SpecialReturn::None => ("return result".to_owned(), type_name(*ty), None),
                    },
                    _ => unreachable!(),
                }
            };

        let class_method = if let wit_parser::FunctionKind::Static(_)
        | wit_parser::FunctionKind::AsyncStatic(_) = function.wit_kind
        {
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
            | Type::F32
            | Type::F64
            | Type::String
            | Type::ErrorContext => (),
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

                                for ty in function.result.types() {
                                    self.sort(ty, &mut *sorted, &mut *visited);
                                }
                            };

                            let empty = &ResourceInfo::default();

                            if self.resource_info.get(&id).unwrap_or(empty).remote {
                                for function in &self.functions {
                                    if matches_resource(function, id, Direction::Import) {
                                        sort(function, sorted, visited);
                                    }
                                }
                            }

                            if self.resource_info.get(&id).unwrap_or(empty).local {
                                for function in &self.functions {
                                    if matches_resource(function, id, Direction::Export) {
                                        sort(function, sorted, visited);
                                    }
                                }
                            }

                            sorted.insert(id);
                        }
                    }
                    TypeDefKind::Stream(ty) | TypeDefKind::Future(ty) => {
                        if let Some(ty) = ty {
                            self.sort(*ty, sorted, visited);
                        }
                        sorted.insert(id);
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

    fn interface_names(
        &self,
        ids: impl Iterator<Item = InterfaceId>,
        interface_names: &HashMap<&str, &str>,
    ) -> HashMap<InterfaceId, String> {
        let mut tree = HashMap::<_, HashMap<_, HashMap<_, _>>>::new();
        for id in ids {
            let info = if let Some(info) = self.imported_interfaces.get(&id) {
                info
            } else if let Some(info) = self.exported_interfaces.get(&id) {
                info
            } else {
                unreachable!()
            };

            assert!(
                tree.entry(info.name)
                    .or_default()
                    .entry(info.package.map(|p| (p.namespace, p.name)))
                    .or_default()
                    .insert(info.package.and_then(|p| p.version), id)
                    .is_none()
            );
        }

        let mut names = HashMap::new();
        for (name, packages) in &tree {
            for (package, versions) in packages {
                if let Some((package_namespace, package_name)) = package {
                    for (version, id) in versions {
                        assert!(
                            names
                                .insert(
                                    *id,
                                    if let Some(version) = version {
                                        if let Some(name) = interface_names.get(
                                        format!(
                                            "{package_namespace}:{package_name}/{name}@{version}"
                                        )
                                        .as_str(),
                                    ) {
                                        (*name).to_owned()
                                    } else if versions.len() == 1 {
                                        if packages.len() == 1 {
                                            (*name).to_owned()
                                        } else {
                                            format!("{package_namespace}-{package_name}-{name}")
                                        }
                                    } else {
                                        format!(
                                            "{package_namespace}-{package_name}-{name}-{}",
                                            version.to_string().replace('.', "-")
                                        )
                                    }
                                    } else if let Some(name) = interface_names.get(
                                        format!("{package_namespace}:{package_name}/{name}")
                                            .as_str()
                                    ) {
                                        (*name).to_owned()
                                    } else if packages.len() == 1 {
                                        (*name).to_owned()
                                    } else {
                                        format!("{package_namespace}-{package_name}-{name}",)
                                    }
                                )
                                .is_none()
                        );
                    }
                } else {
                    assert!(
                        names
                            .insert(
                                *versions.get(&None).unwrap(),
                                (*interface_names.get(*name).unwrap_or(name)).to_owned()
                            )
                            .is_none()
                    );
                }
            }
        }

        names
    }

    #[allow(clippy::too_many_arguments)]
    fn generate_export_code(
        &self,
        stub_runtime_calls: bool,
        function: &MyFunction,
        class_method: &str,
        snake: &str,
        params: &str,
        return_type: &str,
        docs: &str,
    ) -> String {
        let (skip_count, async_prefix) = match function.wit_kind {
            wit_parser::FunctionKind::Freestanding => (0, None),
            wit_parser::FunctionKind::AsyncFreestanding => (0, Some("self.")),
            wit_parser::FunctionKind::Constructor(_) => (0, None),
            wit_parser::FunctionKind::Method(_) => (1, None),
            wit_parser::FunctionKind::AsyncMethod(_) => (1, Some("self.")),
            wit_parser::FunctionKind::Static(_) => (0, None),
            wit_parser::FunctionKind::AsyncStatic(_) => (0, Some("cls.")),
        };

        let (async_, body) = if let Some(prefix) = async_prefix {
            let args = function
                .params
                .iter()
                .skip(skip_count)
                .map(|(name, _)| name.to_snake_case().escape())
                .collect::<Vec<_>>()
                .join(", ");

            (
                "async ",
                if stub_runtime_calls {
                    NOT_IMPLEMENTED.into()
                } else {
                    let index = self.exported_function_index(function);

                    format!(
                        "{NOT_IMPLEMENTED}

{class_method}
    def {ASYNC_START_PREFIX}{snake}({params}) -> int:
        return componentize_py_async_support.first_poll({index}, {prefix}{snake}({args}))"
                    )
                },
            )
        } else {
            ("", NOT_IMPLEMENTED.into())
        };

        format!(
            "{class_method}
    @abstractmethod
    {async_}def {snake}({params}){return_type}:
        {docs}{body}
"
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn generate_import_code(
        &self,
        indent_level: usize,
        stub_runtime_calls: bool,
        function: &MyFunction,
        class_method: &str,
        snake: &str,
        params: &str,
        return_type: &str,
        docs: &str,
        args: &str,
        return_statement: &str,
    ) -> String {
        let index = if stub_runtime_calls {
            0
        } else {
            self.imported_function_index(function)
        };

        let indent = (0..indent_level)
            .map(|_| "    ")
            .collect::<Vec<_>>()
            .concat();

        let (async_, await_) = if let wit_parser::FunctionKind::AsyncFreestanding
        | wit_parser::FunctionKind::AsyncMethod(_)
        | wit_parser::FunctionKind::AsyncStatic(_) = function.wit_kind
        {
            (
                "async ",
                format!(
                    "result = await componentize_py_async_support.await_result(result)\n{indent}    "
                ),
            )
        } else {
            ("", String::new())
        };

        if stub_runtime_calls {
            format!(
                "{class_method}
{indent}{async_}def {snake}({params}){return_type}:
{indent}    {docs}{NOT_IMPLEMENTED}"
            )
        } else {
            format!(
                "{class_method}
{indent}{async_}def {snake}({params}){return_type}:
{indent}    {docs}result = componentize_py_runtime.call_import({index}, [{args}])
{indent}    {await_}{return_statement}"
            )
        }
    }

    pub fn generate_code(
        &self,
        path: &Path,
        world: WorldId,
        world_module: &str,
        locations: &mut Locations,
        stub_runtime_calls: bool,
    ) -> Result<()> {
        #[derive(Default)]
        struct Definitions<'a> {
            types: Vec<String>,
            functions: Vec<String>,
            type_imports: HashSet<InterfaceId>,
            function_imports: HashSet<InterfaceId>,
            docs: Option<&'a str>,
            alias_module: Option<String>,
        }

        let file_header = "# This file is automatically generated by componentize-py
# It is not intended for manual editing.
";

        let mut interface_imports = HashMap::<InterfaceId, Definitions>::new();
        let mut interface_exports = HashMap::<InterfaceId, Definitions>::new();
        let mut world_imports = Definitions::default();
        let mut world_exports = Definitions::default();
        let mut seen = HashSet::new();
        let mut stream_payloads = HashSet::new();
        let mut future_payloads = HashSet::new();
        for (index, id) in self.types.iter().copied().enumerate() {
            if !self
                .world_types
                .get(&world)
                .map(|types| types.contains(&id))
                .unwrap_or(false)
            {
                continue;
            }

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
                    "pass".to_owned().clone_into(&mut fields)
                }

                let docs = docstring(world_module, docs, 1, None);

                format!(
                    "
@dataclass
class {name}:
    {docs}{fields}
"
                )
            };

            let code = if let Some(location) = locations.types.get(&id) {
                location.aliases.clone()
            } else {
                let (code, names) = match &ty.kind {
                    TypeDefKind::Record(record) => (
                        Some(Code::Shared(make_class(
                            &mut names,
                            camel(),
                            ty.docs.contents.as_deref(),
                            record
                                .fields
                                .iter()
                                .map(|field| (field.name.to_snake_case().escape(), field.ty))
                                .collect::<Vec<_>>(),
                        ))),
                        vec![camel()],
                    ),
                    TypeDefKind::Variant(variant) => {
                        let camel = camel();
                        let classes = variant
                            .cases
                            .iter()
                            .map(|case| {
                                make_class(
                                    &mut names,
                                    format!("{camel}_{}", case.name.to_upper_camel_case().escape()),
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
                            .map(|case| {
                                format!("{camel}_{}", case.name.to_upper_camel_case().escape())
                            })
                            .collect::<Vec<_>>()
                            .join(", ");

                        let docs = docstring(world_module, ty.docs.contents.as_deref(), 0, None);

                        (
                            Some(Code::Shared(format!(
                                "
{classes}

{camel} = Union[{cases}]
{docs}
"
                            ))),
                            variant
                                .cases
                                .iter()
                                .map(|case| {
                                    format!("{camel}{}", case.name.to_upper_camel_case().escape())
                                })
                                .collect::<Vec<_>>()
                                .into_iter()
                                .chain(iter::once(camel))
                                .collect(),
                        )
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

                        let docs = docstring(world_module, ty.docs.contents.as_deref(), 1, None);

                        (
                            Some(Code::Shared(format!(
                                "
class {camel}(Enum):
    {docs}{cases}
"
                            ))),
                            vec![camel],
                        )
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

                        let docs = docstring(world_module, ty.docs.contents.as_deref(), 1, None);

                        (
                            Some(Code::Shared(format!(
                                "
class {camel}(Flag):
    {docs}{flags}
"
                            ))),
                            vec![camel],
                        )
                    }
                    TypeDefKind::Resource => {
                        let camel = camel();

                        let docs = docstring(world_module, ty.docs.contents.as_deref(), 1, None);

                        let empty = &ResourceInfo::default();

                        let import = if self.resource_info.get(&id).unwrap_or(empty).remote {
                            let method = |function| {
                                let FunctionCode {
                                    snake,
                                    params,
                                    args,
                                    return_type,
                                    return_statement,
                                    class_method,
                                    error,
                                } = self.function_code(
                                    Direction::Import,
                                    world_module,
                                    function,
                                    &mut names,
                                    &seen,
                                    Some(id),
                                );

                                let docs =
                                    docstring(world_module, function.docs, 2, error.as_deref());

                                if let wit_parser::FunctionKind::Constructor(_) = function.wit_kind
                                {
                                    if stub_runtime_calls {
                                        format!(
                                            "
    def {snake}({params}){return_type}:
        {docs}{NOT_IMPLEMENTED}
"
                                        )
                                    } else {
                                        let index = self.imported_function_index(function);
                                        format!(
                                            "
    def {snake}({params}){return_type}:
        {docs}tmp = componentize_py_runtime.call_import({index}, [{args}])
        (_, func, args, _) = tmp.finalizer.detach()
        self.handle = tmp.handle
        self.finalizer = weakref.finalize(self, func, args[0], args[1])
"
                                        )
                                    }
                                } else {
                                    self.generate_import_code(
                                        1,
                                        stub_runtime_calls,
                                        function,
                                        class_method,
                                        &snake,
                                        &params,
                                        &return_type,
                                        &docs,
                                        &args,
                                        &return_statement,
                                    )
                                }
                            };

                            let methods = self
                                .functions
                                .iter()
                                .filter(move |function| matches_resource(function, id, Direction::Import))
                                .map(method)
                                .chain(iter::once({
                                    let newline = '\n';
                                    let indent = "        ";
                                    let doc = "Release this resource.";
                                    let docs =
                                        format!(r#""""{newline}{indent}{doc}{newline}{indent}"""{newline}{indent}"#);
                                    let enter = r#"
    def __enter__(self) -> Self:
        """Returns self"""
        return self
                                "#;
                                    if stub_runtime_calls {
                                        format!(
                                            "{enter}
    def __exit__(self, exc_type: type[BaseException] | None, exc_value: BaseException | None, traceback: TracebackType | None) -> bool | None:
        {docs}{NOT_IMPLEMENTED}
"
                                        )
                                    } else {
                                        format!(
                                            "{enter}
    def __exit__(self, exc_type: type[BaseException] | None, exc_value: BaseException | None, traceback: TracebackType | None) -> bool | None:
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

                        let export = if self.resource_info.get(&id).unwrap_or(empty).local {
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

                                let docs =
                                    docstring(world_module, function.docs, 2, error.as_deref());

                                self.generate_export_code(
                                    stub_runtime_calls,
                                    function,
                                    class_method,
                                    &snake,
                                    &params,
                                    &return_type,
                                    &docs,
                                )
                            };

                            let methods = self
                                .functions
                                .iter()
                                .filter(|function| {
                                    matches_resource(function, id, Direction::Export)
                                })
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

                        (Some(Code::Separate { import, export }), vec![camel])
                    }
                    TypeDefKind::Tuple(_)
                    | TypeDefKind::List(_)
                    | TypeDefKind::Option(_)
                    | TypeDefKind::Result(_)
                    | TypeDefKind::Handle(_) => (None, Vec::new()),
                    TypeDefKind::Stream(ty) => {
                        let code = if stream_payloads.contains(ty) {
                            None
                        } else {
                            stream_payloads.insert(ty);

                            Some(if let Some(Type::U8 | Type::S8) = ty {
                                if stub_runtime_calls {
                                    format!(
                                        "
def byte_stream() -> tuple[ByteStreamWriter, ByteStreamReader]:
    {NOT_IMPLEMENTED}
"
                                    )
                                } else {
                                    let index = *self.stream_and_future_indexes.get(&id).unwrap();
                                    format!(
                                        "
def byte_stream() -> tuple[ByteStreamWriter, ByteStreamReader]:
    pair = componentize_py_runtime.stream_new({index})
    return (ByteStreamWriter({index}, pair >> 32), ByteStreamReader({index}, pair & 0xFFFFFFFF))
"
                                    )
                                }
                            } else {
                                let snake = ty
                                    .map(|ty| names.mangle_name(ty))
                                    .unwrap_or_else(|| "unit".into());
                                let camel = ty
                                    .map(|ty| names.type_name(ty, &seen, None))
                                    .unwrap_or_else(|| "None".into());
                                if stub_runtime_calls {
                                    format!(
                                        "
def {snake}_stream() -> tuple[StreamWriter[{camel}], StreamReader[{camel}]]:
    {NOT_IMPLEMENTED}
"
                                    )
                                } else {
                                    let index = *self.stream_and_future_indexes.get(&id).unwrap();
                                    format!(
                                        "
def {snake}_stream() -> tuple[StreamWriter[{camel}], StreamReader[{camel}]]:
    pair = componentize_py_runtime.stream_new({index})
    return (StreamWriter({index}, pair >> 32), StreamReader({index}, pair & 0xFFFFFFFF))
"
                                    )
                                }
                            })
                        };

                        (code.map(Code::Shared), Vec::new())
                    }
                    TypeDefKind::Future(ty) => {
                        let code = if future_payloads.contains(ty) {
                            None
                        } else {
                            future_payloads.insert(ty);

                            let snake = ty
                                .map(|ty| names.mangle_name(ty))
                                .unwrap_or_else(|| "unit".into());
                            let camel = ty
                                .map(|ty| names.type_name(ty, &seen, None))
                                .unwrap_or_else(|| "None".into());
                            Some(if stub_runtime_calls {
                                format!(
                                    "
def {snake}_future(default: Callable[[], {camel}]) -> tuple[FutureWriter[{camel}], FutureReader[{camel}]]:
    {NOT_IMPLEMENTED}
"
                                )
                            } else {
                                let index = *self.stream_and_future_indexes.get(&id).unwrap();
                                format!(
                                "
def {snake}_future(default: Callable[[], {camel}]) -> tuple[FutureWriter[{camel}], FutureReader[{camel}]]:
    pair = componentize_py_runtime.future_new({index})
    return (FutureWriter({index}, pair >> 32, default), FutureReader({index}, pair & 0xFFFFFFFF))
"
                            )
                            })
                        };

                        (code.map(Code::Shared), Vec::new())
                    }
                    kind => todo!("{kind:?}"),
                };

                let code = match code {
                    Some(Code::Shared(code))
                        if self.has_imported_and_exported_resource(Type::Id(id)) =>
                    {
                        Some(Code::Separate {
                            import: Some(code.clone()),
                            export: Some(code),
                        })
                    }
                    code => code,
                };

                let aliases = if let (Some(code), false) = (code.as_ref(), names.is_empty()) {
                    let aliases = iter::once(world_module_import(world_module, "peer"))
                        .chain(names.iter().map(|name| format!("{name} = peer.{name}")))
                        .collect::<Vec<_>>()
                        .join("\n");

                    Some(match code {
                        Code::Shared(_) => Code::Shared(aliases),
                        Code::Separate { import, export } => Code::Separate {
                            import: import.as_ref().map(|_| aliases.clone()),
                            export: export.as_ref().map(|_| aliases.clone()),
                        },
                    })
                } else {
                    None
                };

                locations.types.insert(
                    id,
                    TypeLocation {
                        module: world_module.to_owned(),
                        aliases,
                    },
                );

                code
            };

            if let Some(code) = code {
                let tuples = match ty.owner {
                    TypeOwner::Interface(interface) => match code {
                        Code::Shared(code) => vec![(
                            code,
                            if let Some(info) = self.imported_interfaces.get(&interface) {
                                (interface_imports.entry(interface).or_default(), info.docs)
                            } else if let Some(info) = self.exported_interfaces.get(&interface) {
                                (interface_exports.entry(interface).or_default(), info.docs)
                            } else {
                                unreachable!()
                            },
                        )],
                        Code::Separate { import, export } => import
                            .map(|code| {
                                let info = self.imported_interfaces.get(&interface).unwrap();
                                (
                                    code,
                                    (interface_imports.entry(interface).or_default(), info.docs),
                                )
                            })
                            .into_iter()
                            .chain(export.map(|code| {
                                let info = self.exported_interfaces.get(&interface).unwrap();
                                (
                                    code,
                                    (interface_exports.entry(interface).or_default(), info.docs),
                                )
                            }))
                            .collect(),
                    },

                    TypeOwner::World(_) | TypeOwner::None => {
                        let docs = self.resolve.worlds[world].docs.contents.as_deref();
                        match code {
                            Code::Shared(code) => vec![(code, (&mut world_exports, docs))],
                            Code::Separate { import, export } => import
                                .map(|code| (code, (&mut world_imports, docs)))
                                .into_iter()
                                .chain(export.map(|code| (code, (&mut world_exports, docs))))
                                .collect(),
                        }
                    }
                };

                for (code, (definitions, docs)) in tuples {
                    definitions.types.push(code);
                    definitions.type_imports.extend(names.imports.clone());
                    definitions.docs = docs;
                }
            }

            seen.insert(id);
        }

        for function in &self.functions {
            let key = function.key();
            let direction = if let FunctionKind::Import = &function.kind {
                Direction::Import
            } else {
                Direction::Export
            };

            #[allow(clippy::single_match)]
            match (
                &function.kind,
                &function.wit_kind,
                self.world_keys
                    .get(&world)
                    .map(|keys| keys.contains(&(direction, key.clone())))
                    .unwrap_or(false),
            ) {
                (
                    FunctionKind::Import | FunctionKind::Export,
                    wit_parser::FunctionKind::Freestanding
                    | wit_parser::FunctionKind::AsyncFreestanding,
                    true,
                ) => {
                    let mut names = TypeNames::new(
                        self,
                        if let FunctionKind::Export = function.kind {
                            TypeOwner::None
                        } else if let Some(interface) = &function.interface {
                            TypeOwner::Interface(interface.id)
                        } else {
                            TypeOwner::World(world)
                        },
                    );

                    let FunctionCode {
                        snake,
                        params,
                        args,
                        return_type,
                        return_statement,
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
                            let docs = docstring(world_module, function.docs, 1, error.as_deref());

                            let code = self.generate_import_code(
                                0,
                                stub_runtime_calls,
                                function,
                                "",
                                &snake,
                                &params,
                                &return_type,
                                &docs,
                                &args,
                                &return_statement,
                            );

                            let (definitions, docs) = if let Some(interface) = &function.interface {
                                (
                                    interface_imports.entry(interface.id).or_default(),
                                    interface.docs,
                                )
                            } else {
                                (
                                    &mut world_imports,
                                    self.resolve.worlds[world].docs.contents.as_deref(),
                                )
                            };

                            definitions.functions.push(code);
                            definitions.function_imports.extend(names.imports);
                            definitions.docs = docs;
                        }
                        FunctionKind::Export => {
                            let (definitions, docs) = if let Some(interface) = &function.interface {
                                (
                                    interface_exports.entry(interface.id).or_default(),
                                    interface.docs,
                                )
                            } else {
                                (
                                    &mut world_exports,
                                    self.resolve.worlds[world].docs.contents.as_deref(),
                                )
                            };

                            let module = locations
                                .keys
                                .entry(key)
                                .or_insert_with(|| world_module.to_owned());

                            if module == world_module {
                                let params = if params.is_empty() {
                                    "self".to_owned()
                                } else {
                                    format!("self, {params}")
                                };

                                let function_docs =
                                    docstring(world_module, function.docs, 2, error.as_deref());

                                let code = self.generate_export_code(
                                    stub_runtime_calls,
                                    function,
                                    "",
                                    &snake,
                                    &params,
                                    &return_type,
                                    &function_docs,
                                );

                                definitions.functions.push(code);
                                definitions.function_imports.extend(names.imports);
                                definitions.docs = docs;
                            } else {
                                definitions.alias_module = Some(module.clone());
                            }
                        }
                    }
                }
                _ => (),
            }
        }

        let python_imports =
            "from typing import TypeVar, Generic, Union, Optional, Protocol, Tuple, List, Any, Self, Callable
from types import TracebackType
from enum import Flag, Enum, auto
from dataclasses import dataclass
from abc import abstractmethod
import weakref
";

        let async_imports = "import componentize_py_async_support
from componentize_py_async_support.streams import StreamReader, StreamWriter, ByteStreamReader, ByteStreamWriter
from componentize_py_async_support.futures import FutureReader, FutureWriter";

        let import = |prefix, interface| {
            let (module, package) = self.interface_package(interface);
            format!("from {prefix}{module} import {package}")
        };

        if !interface_imports.is_empty() {
            let dir = path.join("imports");
            fs::create_dir(&dir)?;
            File::create(dir.join("__init__.py"))?;
            for (id, code) in interface_imports {
                let name = self.imported_interface_names.get(&id).unwrap();
                let mut file =
                    File::create(dir.join(format!("{}.py", name.to_snake_case().escape())))?;
                let types = code.types.concat();
                let functions = code.functions.concat();
                let imports = code
                    .type_imports
                    .union(&code.function_imports)
                    .map(|&interface| import("..", interface))
                    .chain(self.need_async.then(|| async_imports.into()))
                    .chain((!stub_runtime_calls).then(|| "import componentize_py_runtime".into()))
                    .collect::<Vec<_>>()
                    .join("\n");
                let docs = docstring(world_module, code.docs, 0, None);

                write!(
                    file,
                    "{file_header}{docs}{python_imports}
from componentize_py_types import Result, Ok, Err, Some
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
            for (id, code) in interface_exports {
                let name = self.exported_interface_names.get(&id).unwrap();
                let mut file =
                    File::create(dir.join(format!("{}.py", name.to_snake_case().escape())))?;
                let types = code.types.concat();
                let imports = code
                    .type_imports
                    .into_iter()
                    .map(|interface| import("..", interface))
                    .chain(self.need_async.then(|| async_imports.into()))
                    .collect::<Vec<_>>()
                    .join("\n");
                let docs = docstring(world_module, code.docs, 0, None);

                write!(
                    file,
                    "{file_header}{docs}{python_imports}
from componentize_py_types import Result, Ok, Err, Some
{imports}
{types}
"
                )?;

                let camel = name.to_upper_camel_case().escape();

                if let Some(alias_module) = code.alias_module {
                    writeln!(
                        &mut protocols,
                        "import {}",
                        if let Some((start, _)) = alias_module.split_once('.') {
                            start
                        } else {
                            &alias_module
                        }
                    )?;
                    writeln!(&mut protocols, "{camel} = {alias_module}.{camel}")?;
                } else {
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
            }

            let mut init = File::create(dir.join("__init__.py"))?;
            let imports = protocol_imports
                .into_iter()
                .map(|interface| import("..", interface))
                .chain(self.need_async.then(|| async_imports.into()))
                .collect::<Vec<_>>()
                .join("\n");

            write!(
                init,
                "{file_header}{python_imports}
from componentize_py_types import Result, Ok, Err, Some
{imports}
{protocols}
"
            )?;
        }

        {
            let mut file = File::create(path.join("__init__.py"))?;
            let function_imports = world_imports.functions.concat();
            let type_exports = world_exports.types.concat();
            let camel = world_module.to_upper_camel_case().escape();

            let protocol = if let Some(alias_module) = world_exports.alias_module {
                format!("{camel} = {alias_module}.{camel}")
            } else {
                let methods = if world_exports.functions.is_empty() {
                    "    pass".to_owned()
                } else {
                    world_exports.functions.concat()
                };

                format!(
                    "class {camel}(Protocol):
{methods}"
                )
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
                .chain(self.need_async.then(|| async_imports.into()))
                .chain((!stub_runtime_calls).then(|| "import componentize_py_runtime".into()))
                .collect::<Vec<_>>()
                .join("\n");

            let docs = docstring(world_module, world_exports.docs, 0, None);

            write!(
                file,
                "{file_header}{docs}{python_imports}
from componentize_py_types import Result, Ok, Err, Some
{imports}
{type_exports}
{function_imports}
{protocol}
"
            )?;
        }

        Ok(())
    }

    fn interface_package(&self, interface: InterfaceId) -> (&'static str, String) {
        if let Some(name) = self.imported_interface_names.get(&interface) {
            ("imports", name.to_snake_case().escape())
        } else {
            (
                "exports",
                self.exported_interface_names[&interface]
                    .to_snake_case()
                    .escape(),
            )
        }
    }

    fn package(
        &self,
        owner: TypeOwner,
        world_module: &str,
        reverse_cloned_interfaces: &HashMap<InterfaceId, InterfaceId>,
    ) -> Option<String> {
        match owner {
            TypeOwner::Interface(mut interface) => {
                if let Some(&original) = reverse_cloned_interfaces.get(&interface) {
                    interface = original;
                }
                let (module, package) = self.interface_package(interface);
                Some(format!("{world_module}.{module}.{package}"))
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
            | Type::F32
            | Type::F64
            | Type::String
            | Type::ErrorContext => false,
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
                    info.local && info.remote
                }
                TypeDefKind::Stream(ty) | TypeDefKind::Future(ty) => ty
                    .map(|ty| self.has_imported_and_exported_resource(ty))
                    .unwrap_or(false),
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
            Type::Bool => "bool".into(),
            Type::U8
            | Type::U16
            | Type::U32
            | Type::U64
            | Type::S8
            | Type::S16
            | Type::S32
            | Type::S64
            | Type::ErrorContext => "int".into(),
            Type::F32 | Type::F64 => "float".into(),
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
                                    // todo: place anonymous types in types.py
                                    // and import them from there
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
                            // As of this writing, there's no concept of forward
                            // declaration in Python, so we must either use
                            // `Any` or `Self` for types which have not yet been
                            // fully declared.
                            if Some(id) == resource { "Self" } else { "Any" }.to_owned()
                        }
                    }
                    TypeDefKind::Option(some) => {
                        if is_option(self.summary.resolve, *some) {
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
                        format!("Tuple[{types}]")
                    }
                    TypeDefKind::Handle(Handle::Own(ty) | Handle::Borrow(ty)) => {
                        self.type_name(Type::Id(*ty), seen, resource)
                    }
                    TypeDefKind::Type(ty) => self.type_name(*ty, seen, resource),
                    TypeDefKind::Stream(ty) => {
                        if let Some(Type::U8 | Type::S8) = ty {
                            "ByteStreamReader".into()
                        } else {
                            format!(
                                "StreamReader[{}]",
                                ty.map(|ty| self.type_name(ty, seen, resource))
                                    .unwrap_or_else(|| "None".into())
                            )
                        }
                    }
                    TypeDefKind::Future(ty) => {
                        format!(
                            "FutureReader[{}]",
                            ty.map(|ty| self.type_name(ty, seen, resource))
                                .unwrap_or_else(|| "None".into())
                        )
                    }
                    kind => todo!("{kind:?}"),
                }
            }
        }
    }

    fn mangle_name(&mut self, ty: Type) -> String {
        // TODO: Ensure the returned name is always distinct for distinct types
        // (e.g. by incorporating interface version numbers and/or additional
        // mangling as needed).
        match ty {
            Type::Bool => "bool".into(),
            Type::U8 => "u8".into(),
            Type::U16 => "u16".into(),
            Type::U32 => "u32".into(),
            Type::U64 => "u64".into(),
            Type::S8 => "s8".into(),
            Type::S16 => "s16".into(),
            Type::S32 => "s32".into(),
            Type::S64 => "s64".into(),
            Type::ErrorContext => "error_context".into(),
            Type::F32 => "f32".into(),
            Type::F64 => "f64".into(),
            Type::Char => "char".into(),
            Type::String => "string".into(),
            Type::Id(id) => {
                let ty = &self.summary.resolve.types[id];
                match &ty.kind {
                    TypeDefKind::Record(_)
                    | TypeDefKind::Variant(_)
                    | TypeDefKind::Enum(_)
                    | TypeDefKind::Flags(_)
                    | TypeDefKind::Resource => {
                        let package = if ty.owner == self.owner {
                            String::new()
                        } else {
                            match ty.owner {
                                TypeOwner::Interface(interface) => {
                                    format!("{}_", self.summary.interface_package(interface).1)
                                }
                                _ => String::new(),
                            }
                        };

                        let name = if let Some(name) = &ty.name {
                            name.to_snake_case().escape()
                        } else {
                            format!("anon{}", self.summary.types.get_index_of(&id).unwrap())
                        };

                        format!("{package}{name}")
                    }
                    TypeDefKind::Option(some) => {
                        format!("option_{}", self.mangle_name(*some))
                    }
                    TypeDefKind::Result(result) => format!(
                        "result_{}_{}",
                        result
                            .ok
                            .map(|ty| self.mangle_name(ty))
                            .unwrap_or_else(|| "unit".into()),
                        result
                            .err
                            .map(|ty| self.mangle_name(ty))
                            .unwrap_or_else(|| "unit".into())
                    ),
                    TypeDefKind::List(ty) => {
                        format!("list_{}", self.mangle_name(*ty))
                    }
                    TypeDefKind::Tuple(tuple) => {
                        let types = tuple
                            .types
                            .iter()
                            .map(|ty| self.mangle_name(*ty))
                            .collect::<Vec<_>>()
                            .join("_");
                        format!("tuple{}_{types}", tuple.types.len())
                    }
                    TypeDefKind::Handle(Handle::Own(ty) | Handle::Borrow(ty)) => {
                        self.mangle_name(Type::Id(*ty))
                    }
                    TypeDefKind::Type(ty) => self.mangle_name(*ty),
                    TypeDefKind::Stream(ty) => {
                        format!(
                            "stream_{}",
                            ty.map(|ty| self.mangle_name(ty))
                                .unwrap_or_else(|| "unit".into())
                        )
                    }
                    TypeDefKind::Future(ty) => {
                        format!(
                            "stream_{}",
                            ty.map(|ty| self.mangle_name(ty))
                                .unwrap_or_else(|| "unit".into())
                        )
                    }
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
        // Escape Python keywords; source:
        // https://docs.python.org/3/reference/lexical_analysis.html#keywords
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
                wit_parser::FunctionKind::Freestanding
                | wit_parser::FunctionKind::AsyncFreestanding => false,
                wit_parser::FunctionKind::Method(id)
                | wit_parser::FunctionKind::AsyncMethod(id)
                | wit_parser::FunctionKind::Static(id)
                | wit_parser::FunctionKind::AsyncStatic(id)
                | wit_parser::FunctionKind::Constructor(id) => *id == resource,
            }
        }
        _ => false,
    }
}

fn world_module_import(name: &str, alias: &str) -> String {
    if let Some((front, rear)) = name.rsplit_once('.') {
        format!("from {front} import {rear} as {alias}")
    } else {
        format!("import {name} as {alias}")
    }
}

fn docstring(
    world_module: &str,
    docs: Option<&str>,
    indent_level: usize,
    error: Option<&str>,
) -> String {
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
}

fn dealias(resolve: &Resolve, mut id: TypeId) -> TypeId {
    loop {
        match &resolve.types[id].kind {
            TypeDefKind::Type(Type::Id(that_id)) => id = *that_id,
            _ => break id,
        }
    }
}

fn is_option(resolve: &Resolve, ty: Type) -> bool {
    if let Type::Id(id) = ty {
        match &resolve.types[id].kind {
            TypeDefKind::Option(_) => true,
            TypeDefKind::Type(ty) => is_option(resolve, *ty),
            _ => false,
        }
    } else {
        false
    }
}

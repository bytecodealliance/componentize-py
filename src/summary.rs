use {
    crate::{
        abi::{self, MAX_FLAT_PARAMS, MAX_FLAT_RESULTS},
        bindgen::DISPATCHABLE_CORE_PARAM_COUNT,
        exports::exports::{
            self, Case, FunctionExport, OwnedKind, OwnedType, RawUnionType, Symbols,
        },
        util::Types as _,
    },
    anyhow::{bail, Result},
    heck::{ToShoutySnakeCase, ToSnakeCase, ToUpperCamelCase},
    indexmap::{IndexMap, IndexSet},
    std::{
        collections::{hash_map::Entry, HashMap, HashSet},
        fmt::Write as _,
        fs::{self, File},
        io::Write as _,
        path::Path,
        str,
    },
    wasm_encoder::ValType,
    wit_parser::{
        InterfaceId, Resolve, Result_, Results, Type, TypeDefKind, TypeId, TypeOwner, Union,
        WorldId, WorldItem, WorldKey,
    },
};

#[derive(Copy, Clone)]
enum Direction {
    Import,
    Export,
}

pub enum FunctionKind {
    Import,
    Export,
    ExportLift,
    ExportLower,
    ExportPostReturn,
}

#[derive(Copy, Clone)]
pub struct PackageName<'a> {
    pub namespace: &'a str,
    pub name: &'a str,
}

#[derive(Copy, Clone)]
pub struct MyInterface<'a> {
    pub id: InterfaceId,
    pub package: Option<PackageName<'a>>,
    pub name: &'a str,
    pub docs: Option<&'a str>,
}

pub struct MyFunction<'a> {
    pub kind: FunctionKind,
    pub interface: Option<MyInterface<'a>>,
    pub name: &'a str,
    pub docs: Option<&'a str>,
    pub params: &'a [(String, Type)],
    pub results: &'a Results,
}

impl<'a> MyFunction<'a> {
    pub fn internal_name(&self) -> String {
        if let Some(interface) = self.interface {
            format!(
                "{}{}#{}{}",
                if let Some(package) = interface.package {
                    format!("{}:{}/", package.namespace, package.name)
                } else {
                    String::new()
                },
                interface.name,
                self.name,
                match self.kind {
                    FunctionKind::Import => "-import",
                    FunctionKind::Export => "-export",
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

pub struct InterfaceInfo<'a> {
    name: &'a str,
    docs: Option<&'a str>,
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
                TypeDefKind::Enum(_) | TypeDefKind::Flags(_) => {
                    self.types.insert(id);
                }
                TypeDefKind::Union(un) => {
                    for case in &un.cases {
                        self.visit_type(case.ty);
                    }
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
                TypeDefKind::List(ty) | TypeDefKind::Type(ty) => {
                    self.visit_type(*ty);
                }
                kind => todo!("{kind:?}"),
            },
        }
    }

    fn visit_function(
        &mut self,
        interface: Option<MyInterface<'a>>,
        name: &'a str,
        docs: Option<&'a str>,
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
            docs,
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
                            }),
                            func_name,
                            func.docs.contents.as_deref(),
                            &func.params,
                            &func.results,
                            direction,
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
                    );
                }

                WorldItem::Type(ty) => self.visit_type(Type::Id(*ty)),
            }
        }
        Ok(())
    }

    fn summarize_type(&self, id: TypeId) -> exports::Type {
        let ty = &self.resolve.types[id];
        if let Some(package) = self.package(ty.owner) {
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
                TypeDefKind::Union(un) => {
                    if self.is_raw_union(un) {
                        OwnedKind::RawUnion(un.cases.iter().map(|c| raw_union_type(c.ty)).collect())
                    } else {
                        OwnedKind::Variant(
                            (0..un.cases.len())
                                .map(|index| Case {
                                    name: format!("{name}{index}"),
                                    has_payload: true,
                                })
                                .collect(),
                        )
                    }
                }
                TypeDefKind::Flags(flags) => {
                    OwnedKind::Flags(flags.repr().count().try_into().unwrap())
                }
                TypeDefKind::Tuple(_) | TypeDefKind::Option(_) | TypeDefKind::Result(_) => {
                    return self.summarize_unowned_type(id)
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
            kind => todo!("{kind:?}"),
        }
    }

    pub fn collect_symbols(&self) -> Symbols {
        let mut exports = Vec::new();
        for function in &self.functions {
            if let FunctionKind::Export = function.kind {
                exports.push(FunctionExport {
                    protocol: if let Some(interface) = function.interface {
                        interface.name
                    } else {
                        &self.resolve.worlds[self.world].name
                    }
                    .to_upper_camel_case()
                    .escape(),

                    name: function.name.to_snake_case().escape(),
                });
            }
        }

        let mut types = Vec::new();
        for ty in &self.types {
            types.push(self.summarize_type(*ty));
        }

        Symbols {
            types_package: format!(
                "{}.types",
                &self.resolve.worlds[self.world]
                    .name
                    .to_snake_case()
                    .escape()
            ),
            exports,
            types,
        }
    }

    pub fn generate_code(&self, path: &Path) -> Result<()> {
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

        #[derive(Default)]
        struct Definitions<'a> {
            types: Vec<String>,
            functions: Vec<String>,
            type_imports: HashSet<InterfaceId>,
            function_imports: HashSet<InterfaceId>,
            docs: Option<&'a str>,
        }

        let docstring = |docs: Option<&str>, indent_level| {
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
        for (index, id) in self.types.iter().enumerate() {
            let ty = &self.resolve.types[*id];
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
                        format!("{field_name}: {}", names.type_name(*field_type))
                    })
                    .collect::<Vec<_>>()
                    .join("\n    ");

                if fields.is_empty() {
                    fields = "pass".to_owned()
                }

                let docs = docstring(docs, 1);

                format!(
                    "
@dataclass
class {name}:
    {docs}{fields}
"
                )
            };

            let code = match &ty.kind {
                TypeDefKind::Record(record) => Some(make_class(
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

                    let docs = if let Some(docs) = &ty.docs.contents {
                        docs.lines()
                            .map(|line| format!("# {line}\n"))
                            .collect::<Vec<_>>()
                            .concat()
                    } else {
                        String::new()
                    };

                    Some(format!(
                        "
{classes}

{docs}{camel} = Union[{cases}]
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

                    let docs = docstring(ty.docs.contents.as_deref(), 1);

                    Some(format!(
                        "
class {camel}(Enum):
    {docs}{cases}
"
                    ))
                }
                TypeDefKind::Union(un) => {
                    let camel = camel();

                    let (classes, cases) = if self.is_raw_union(un) {
                        (
                            String::new(),
                            un.cases
                                .iter()
                                .map(|case| names.type_name(case.ty))
                                .collect::<Vec<_>>()
                                .join(", "),
                        )
                    } else {
                        (
                            format!(
                                "{}\n\n",
                                un.cases
                                    .iter()
                                    .enumerate()
                                    .map(|(index, case)| {
                                        make_class(
                                            &mut names,
                                            format!("{camel}{index}"),
                                            None,
                                            vec![("value".into(), case.ty)],
                                        )
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            ),
                            (0..un.cases.len())
                                .map(|index| format!("{camel}{index}"))
                                .collect::<Vec<_>>()
                                .join(", "),
                        )
                    };

                    let docs = if let Some(docs) = &ty.docs.contents {
                        docs.lines()
                            .map(|line| format!("# {line}\n"))
                            .collect::<Vec<_>>()
                            .concat()
                    } else {
                        String::new()
                    };

                    Some(format!(
                        "
{classes}{docs}{camel} = Union[{cases}]
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

                    let docs = docstring(ty.docs.contents.as_deref(), 1);

                    Some(format!(
                        "
class {camel}(Flag):
    {docs}{flags}
"
                    ))
                }
                TypeDefKind::Tuple(_)
                | TypeDefKind::List(_)
                | TypeDefKind::Option(_)
                | TypeDefKind::Result(_) => None,
                _ => todo!(),
            };

            if let Some(code) = code {
                let (definitions, docs) = match ty.owner {
                    TypeOwner::Interface(interface) => {
                        if let Some(info) = self.imported_interfaces.get(&interface) {
                            (interface_imports.entry(info.name).or_default(), info.docs)
                        } else if let Some(info) = self.exported_interfaces.get(&interface) {
                            (interface_exports.entry(info.name).or_default(), info.docs)
                        } else {
                            unreachable!()
                        }
                    }

                    TypeOwner::World(_) => (
                        &mut world_exports,
                        self.resolve.worlds[self.world].docs.contents.as_deref(),
                    ),

                    TypeOwner::None => unreachable!(),
                };

                definitions.types.push(code);
                definitions.type_imports.extend(names.imports);
                definitions.docs = docs;
            }
        }

        let mut index = 0;
        for function in &self.functions {
            #[allow(clippy::single_match)]
            match function.kind {
                FunctionKind::Import | FunctionKind::Export => {
                    let mut names = TypeNames::new(
                        self,
                        if let FunctionKind::Export = function.kind {
                            TypeOwner::None
                        } else if let Some(interface) = function.interface {
                            TypeOwner::Interface(interface.id)
                        } else {
                            TypeOwner::World(self.world)
                        },
                    );

                    let snake = function.name.to_snake_case().escape();

                    let params = function
                        .params
                        .iter()
                        .map(|(name, ty)| {
                            format!(
                                "{}: {}",
                                name.to_snake_case().escape(),
                                names.type_name(*ty)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");

                    let args = function
                        .params
                        .iter()
                        .map(|(name, _)| name.to_snake_case().escape())
                        .collect::<Vec<_>>()
                        .join(", ");

                    let result_types = function.results.types().collect::<Vec<_>>();

                    let (return_statement, return_type) = match result_types.as_slice() {
                        [] => ("return", "None".to_owned()),
                        [ty] => match special_return(*ty) {
                            SpecialReturn::Result(result) => (
                                "if isinstance(result[0], Err):
        raise result[0]
    else:
        return result[0].value",
                                result
                                    .ok
                                    .map(|ty| names.type_name(ty))
                                    .unwrap_or_else(|| "None".into()),
                            ),
                            SpecialReturn::None => ("return result[0]", names.type_name(*ty)),
                        },
                        _ => (
                            "return result",
                            format!(
                                "({})",
                                result_types
                                    .iter()
                                    .map(|ty| names.type_name(*ty))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                        ),
                    };

                    let result_count = result_types.len();

                    match function.kind {
                        FunctionKind::Import => {
                            let docs = docstring(function.docs, 1);

                            let code = format!(
                                "
def {snake}({params}) -> {return_type}:
    {docs}result = componentize_py_runtime.call_import({index}, [{args}], {result_count})
    {return_statement}
"
                            );

                            let (definitions, docs) = if let Some(interface) = function.interface {
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

                            let docs = docstring(function.docs, 2);

                            let code = format!(
                                "
    @abstractmethod
    def {snake}({params}) -> {return_type}:
        {docs}raise NotImplementedError
"
                            );

                            let (definitions, docs) = if let Some(interface) = function.interface {
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
            "from typing import TypeVar, Generic, Union, Optional, Union, Protocol, Tuple, List
from enum import Flag, Enum, auto
from dataclasses import dataclass
from abc import abstractmethod
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
                    .concat();
                let docs = docstring(code.docs, 0);

                write!(
                    file,
                    "{docs}{python_imports}
from ..types import Result, Ok, Err, Some
import componentize_py_runtime
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
                    .concat();
                let docs = docstring(code.docs, 0);

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
                .concat();

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
                .concat();
            let docs = docstring(world_exports.docs, 0);

            write!(
                file,
                "{docs}{python_imports}
from .types import Result, Ok, Err, Some
import componentize_py_runtime
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

    fn package(&self, owner: TypeOwner) -> Option<String> {
        match owner {
            TypeOwner::Interface(interface) => {
                let (module, package) = self.interface_package(interface);
                Some(format!(
                    "{}.{module}.{package}",
                    self.resolve.worlds[self.world]
                        .name
                        .to_snake_case()
                        .escape(),
                ))
            }
            TypeOwner::World(world) => {
                Some(self.resolve.worlds[world].name.to_snake_case().escape())
            }
            TypeOwner::None => None,
        }
    }

    fn is_allowed_for_raw_union(&self, ty: Type) -> bool {
        // Raw unions can't contain options or other raw unions since that can create ambiguity:
        if let Type::Id(id) = ty {
            match &self.resolve.types[id].kind {
                TypeDefKind::Union(un) => !self.is_raw_union(un),
                TypeDefKind::Option(_) => false,
                TypeDefKind::Type(ty) => self.is_allowed_for_raw_union(*ty),
                _ => true,
            }
        } else {
            true
        }
    }

    fn is_raw_union(&self, un: &Union) -> bool {
        un.cases
            .iter()
            .all(|case| self.is_allowed_for_raw_union(case.ty))
            && un.cases.len()
                == un
                    .cases
                    .iter()
                    .map(|case| raw_union_type(case.ty))
                    .collect::<HashSet<_>>()
                    .len()
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

    fn type_name(&mut self, ty: Type) -> String {
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
                    | TypeDefKind::Union(_)
                    | TypeDefKind::Flags(_) => {
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

                        format!("{package}{name}",)
                    }
                    TypeDefKind::Option(some) => {
                        if abi::is_option(self.summary.resolve, *some) {
                            format!("Optional[Some[{}]]", self.type_name(*some))
                        } else {
                            format!("Optional[{}]", self.type_name(*some))
                        }
                    }
                    TypeDefKind::Result(result) => format!(
                        "Result[{}, {}]",
                        result
                            .ok
                            .map(|ty| self.type_name(ty))
                            .unwrap_or_else(|| "None".into()),
                        result
                            .err
                            .map(|ty| self.type_name(ty))
                            .unwrap_or_else(|| "None".into())
                    ),
                    TypeDefKind::List(ty) => {
                        if let Type::U8 | Type::S8 = ty {
                            "bytes".into()
                        } else {
                            format!("List[{}]", self.type_name(*ty))
                        }
                    }
                    TypeDefKind::Tuple(tuple) => {
                        let types = tuple
                            .types
                            .iter()
                            .map(|ty| self.type_name(*ty))
                            .collect::<Vec<_>>()
                            .join(", ");
                        let types = if types.is_empty() {
                            "()".to_owned()
                        } else {
                            types
                        };
                        format!("Tuple[{types}]")
                    }
                    TypeDefKind::Type(ty) => self.type_name(*ty),
                    kind => todo!("{kind:?}"),
                }
            }
        }
    }
}

fn raw_union_type(ty: Type) -> RawUnionType {
    match ty {
        Type::Bool
        | Type::U8
        | Type::U16
        | Type::U32
        | Type::U64
        | Type::S8
        | Type::S16
        | Type::S32
        | Type::S64 => RawUnionType::Int,
        Type::Float32 | Type::Float64 => RawUnionType::Float,
        Type::Char | Type::String => RawUnionType::Str,
        Type::Id(_) => RawUnionType::Other,
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

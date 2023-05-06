use serde::{Deserialize, Serialize};

#[repr(u8)]
pub enum ReturnStyle {
    Normal,
    Result,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct FunctionExport {
    pub protocol: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum RawUnionType {
    Int,
    Float,
    Str,
    Other,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Case {
    pub name: String,
    pub has_payload: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum OwnedKind {
    Record { fields: Vec<String> },
    Variant { cases: Vec<Case> },
    Enum(usize),
    RawUnion { types: Vec<RawUnionType> },
    Flags(usize),
}

#[derive(Serialize, Deserialize, Debug)]
pub enum Type {
    Owned {
        kind: OwnedKind,
        package: String,
        name: String,
    },
    Option,
    NestingOption,
    Result,
    Tuple(usize),
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Symbols {
    pub types_package: String,
    pub exports: Vec<FunctionExport>,
    pub types: Vec<Type>,
}

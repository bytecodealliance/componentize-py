use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Function<'a> {
    #[serde(borrow)]
    pub interface: Option<&'a str>,
    pub name: &'a str,
}

#[derive(Serialize, Deserialize, Copy, Clone)]
pub enum Direction {
    Import,
    Export,
}

#[derive(Serialize, Deserialize)]
pub struct OwnedType<'a> {
    pub direction: Direction,
    pub interface: &'a str,
    #[serde(borrow)]
    pub name: Option<&'a str>,
    #[serde(borrow)]
    pub fields: Vec<&'a str>,
}

#[derive(Serialize, Deserialize)]
pub enum Type<'a> {
    Owned(#[serde(borrow)] OwnedType<'a>),
    Tuple(usize),
}

#[derive(Serialize, Deserialize)]
pub struct Symbols<'a> {
    #[serde(borrow)]
    pub exports: Vec<Function<'a>>,
    #[serde(borrow)]
    pub types: Vec<Type<'a>>,
}

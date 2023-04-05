use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Function<'a> {
    #[serde(with = "serde_bytes")]
    interface: Option<&'a str>,
    #[serde(with = "serde_bytes")]
    name: &'a str,
}

#[derive(Serialize, Deserialize)]
enum Direction {
    Import,
    Export,
}

#[derive(Serialize, Deserialize)]
struct Type<'a> {
    direction: Direction,
    #[serde(with = "serde_bytes")]
    interface: &'a str,
    #[serde(with = "serde_bytes")]
    name: Option<&'a str>,
}

#[derive(Serialize, Deserialize)]
struct Symbols<'a> {
    exports: Vec<Function<'a>>,
    types: Vec<Type<'a>>,
}

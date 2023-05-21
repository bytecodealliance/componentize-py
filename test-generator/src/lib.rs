#![deny(warnings)]

use {
    anyhow::{anyhow, Result},
    proptest::{
        strategy::{Just, Strategy, ValueTree},
        test_runner::{Config, TestRng, TestRunner},
    },
    std::{
        cell::Cell, collections::HashMap, env, fmt::Write, fs, path::PathBuf, process::Command,
        rc::Rc, str::FromStr,
    },
};

const DEFAULT_TEST_COUNT: usize = 10;
const MAX_LIST_SIZE: usize = 100;
// As of this writing, neither `Debug` nor `Strategy` are implemented for tuples of more than twelve elements.
// Technically we could work around this, but it's probably more trouble than it's worth.
const MAX_TUPLE_SIZE: usize = 12;
// See note above about `MAX_TUPLE_SIZE`
const MAX_PARAM_COUNT: usize = 12;
const MAX_FLAG_COUNT: u32 = 100;
const MAX_ENUM_COUNT: u32 = 100;

#[derive(Clone, Debug)]
enum Type {
    Bool,
    U8,
    S8,
    U16,
    S16,
    U32,
    S32,
    U64,
    S64,
    Float32,
    Float64,
    Char,
    String,
    Record {
        id: usize,
        fields: Vec<Type>,
    },
    Variant {
        id: usize,
        cases: Vec<Option<Type>>,
    },
    Flags {
        id: usize,
        count: u32,
    },
    Enum {
        id: usize,
        count: u32,
    },
    Union {
        id: usize,
        cases: Vec<Type>,
    },
    Option(Box<Type>),
    Result {
        ok: Option<Box<Type>>,
        err: Option<Box<Type>>,
    },
    Tuple(Vec<Type>),
    List(Box<Type>),
}

fn union_case_name(ty: &Type) -> String {
    match ty {
        Type::Bool => "Bool".into(),
        Type::U8 => "U8".into(),
        Type::U16 => "U16".into(),
        Type::U32 => "U32".into(),
        Type::U64 => "U64".into(),
        Type::S8 => "I8".into(),
        Type::S16 => "I16".into(),
        Type::S32 => "I32".into(),
        Type::S64 => "I64".into(),
        Type::Float32 => "F32".into(),
        Type::Float64 => "F64".into(),
        Type::Char => "Char".into(),
        Type::String => "String".into(),
        Type::Record { id, .. } => format!("Record{id}Type"),
        Type::Variant { id, .. } => format!("Variant{id}Type"),
        Type::Flags { id, .. } => format!("Flags{id}Type"),
        Type::Enum { id, .. } => format!("Enum{id}Type"),
        Type::Union { id, .. } => format!("Union{id}Type"),
        Type::Option(ty) => format!("Optional{}", union_case_name(ty)),
        Type::Result { .. } => "Result".into(),
        Type::Tuple(_) => "Tuple".into(),
        Type::List(ty) => format!("{}List", union_case_name(ty)),
    }
}

fn union_case_names(types: &[Type]) -> Vec<String> {
    let mut indexes = HashMap::<_, Vec<_>>::new();
    let mut names = types
        .iter()
        .enumerate()
        .map(|(index, ty)| {
            let name = union_case_name(ty);
            indexes.entry(name.clone()).or_default().push(index);
            name
        })
        .collect::<Vec<_>>();

    for indexes in indexes.into_values() {
        if indexes.len() > 1 {
            for (n, index) in indexes.into_iter().enumerate() {
                names[index] = format!("{}{n}", names[index]);
            }
        }
    }

    names
}

fn borrows(ty: &Type) -> bool {
    match ty {
        Type::Bool
        | Type::U8
        | Type::S8
        | Type::U16
        | Type::S16
        | Type::U32
        | Type::S32
        | Type::U64
        | Type::S64
        | Type::Float32
        | Type::Float64
        | Type::Char
        | Type::Flags { .. }
        | Type::Enum { .. } => false,
        Type::String | Type::List(_) => true,
        Type::Record { fields, .. } | Type::Tuple(fields) => fields.iter().any(borrows),
        Type::Variant { cases, .. } => cases
            .iter()
            .any(|ty| ty.as_ref().map(borrows).unwrap_or(false)),
        Type::Union { cases, .. } => cases.iter().any(borrows),
        Type::Option(ty) => borrows(ty),
        Type::Result { ok, err } => {
            ok.as_ref().map(|ty| borrows(ty)).unwrap_or(false)
                || err.as_ref().map(|ty| borrows(ty)).unwrap_or(false)
        }
    }
}

fn any_type(max_size: usize, next_id: Rc<Cell<usize>>) -> impl Strategy<Value = Type> {
    (0..22).prop_flat_map(move |index| match index {
        0 => Just(Type::Bool).boxed(),
        1 => Just(Type::U8).boxed(),
        2 => Just(Type::S8).boxed(),
        3 => Just(Type::U16).boxed(),
        4 => Just(Type::S16).boxed(),
        5 => Just(Type::U32).boxed(),
        6 => Just(Type::S32).boxed(),
        7 => Just(Type::U64).boxed(),
        8 => Just(Type::S64).boxed(),
        9 => Just(Type::Float32).boxed(),
        10 => Just(Type::Float64).boxed(),
        11 => Just(Type::Char).boxed(),
        12 => Just(Type::String).boxed(),
        13 => {
            proptest::collection::vec(any_type(max_size / 2, next_id.clone()), 0..max_size.max(1))
                .prop_map({
                    let next_id = next_id.clone();
                    move |fields| {
                        let id = next_id.get();
                        next_id.set(id + 1);
                        Type::Record { id, fields }
                    }
                })
                .boxed()
        }
        14 => proptest::collection::vec(
            proptest::option::of(any_type(max_size / 2, next_id.clone())),
            1..max_size.max(2),
        )
        .prop_map({
            let next_id = next_id.clone();
            move |cases| {
                let id = next_id.get();
                next_id.set(id + 1);
                Type::Variant { id, cases }
            }
        })
        .boxed(),
        15 => (0..MAX_FLAG_COUNT)
            .prop_map({
                let next_id = next_id.clone();
                move |count| {
                    let id = next_id.get();
                    next_id.set(id + 1);
                    Type::Flags { id, count }
                }
            })
            .boxed(),
        16 => (1..MAX_ENUM_COUNT)
            .prop_map({
                let next_id = next_id.clone();
                move |count| {
                    let id = next_id.get();
                    next_id.set(id + 1);
                    Type::Enum { id, count }
                }
            })
            .boxed(),
        17 => {
            proptest::collection::vec(any_type(max_size / 2, next_id.clone()), 1..max_size.max(2))
                .prop_map({
                    let next_id = next_id.clone();
                    move |cases| {
                        let id = next_id.get();
                        next_id.set(id + 1);
                        Type::Union { id, cases }
                    }
                })
                .boxed()
        }
        18 => any_type(max_size, next_id.clone())
            .prop_map(|ty| Type::Option(Box::new(ty)))
            .boxed(),
        19 => (
            proptest::option::of(any_type(max_size, next_id.clone())),
            proptest::option::of(any_type(max_size, next_id.clone())),
        )
            .prop_map(|(ok, err)| Type::Result {
                ok: ok.map(Box::new),
                err: err.map(Box::new),
            })
            .boxed(),
        20 => {
            proptest::collection::vec(any_type(max_size / 2, next_id.clone()), 0..max_size.max(2))
                .prop_map(Type::Tuple)
                .boxed()
        }
        21 => any_type(max_size, next_id.clone())
            .prop_map(|ty| Type::List(Box::new(ty)))
            .boxed(),
        _ => unreachable!(),
    })
}

fn wit_type_name(wit: &mut String, ty: &Type) -> String {
    match ty {
        Type::Bool => "bool".into(),
        Type::U8 => "u8".into(),
        Type::S8 => "s8".into(),
        Type::U16 => "u16".into(),
        Type::S16 => "s16".into(),
        Type::U32 => "u32".into(),
        Type::S32 => "s32".into(),
        Type::U64 => "u64".into(),
        Type::S64 => "s64".into(),
        Type::Float32 => "float32".into(),
        Type::Float64 => "float64".into(),
        Type::Char => "char".into(),
        Type::String => "string".into(),
        Type::Record { id, fields } => {
            let fields = fields
                .iter()
                .enumerate()
                .map(|(index, ty)| {
                    let ty = wit_type_name(wit, ty);
                    format!("f{index}: {ty}")
                })
                .collect::<Vec<_>>()
                .join(",\n        ");

            write!(
                wit,
                "
    record record{id}-type {{
        {fields}
    }}
"
            )
            .unwrap();

            format!("record{id}-type")
        }
        Type::Variant { id, cases } => {
            let cases = cases
                .iter()
                .enumerate()
                .map(|(index, ty)| {
                    if let Some(ty) = ty {
                        let ty = wit_type_name(wit, ty);
                        format!("c{index}({ty})")
                    } else {
                        format!("c{index}")
                    }
                })
                .collect::<Vec<_>>()
                .join(",\n        ");

            write!(
                wit,
                "
    variant variant{id}-type {{
        {cases}
    }}
"
            )
            .unwrap();

            format!("variant{id}-type")
        }
        Type::Flags { id, count } => {
            let flags = (0..*count)
                .map(|index| format!("f{index}"))
                .collect::<Vec<_>>()
                .join(",\n        ");

            write!(
                wit,
                "
    flags flags{id}-type {{
        {flags}
    }}
"
            )
            .unwrap();

            format!("flags{id}-type")
        }
        Type::Enum { id, count } => {
            let cases = (0..*count)
                .map(|index| format!("c{index}"))
                .collect::<Vec<_>>()
                .join(",\n        ");

            write!(
                wit,
                "
    enum enum{id}-type {{
        {cases}
    }}
"
            )
            .unwrap();

            format!("enum{id}-type")
        }
        Type::Union { id, cases } => {
            let cases = cases
                .iter()
                .map(|ty| wit_type_name(wit, ty))
                .collect::<Vec<_>>()
                .join(",\n        ");

            write!(
                wit,
                "
    union union{id}-type {{
        {cases}
    }}
"
            )
            .unwrap();

            format!("union{id}-type")
        }
        Type::Option(ty) => {
            format!("option<{}>", wit_type_name(wit, ty))
        }
        Type::Result { ok, err } => {
            let ok = ok.as_ref().map(|ty| wit_type_name(wit, ty));
            let err = err.as_ref().map(|ty| wit_type_name(wit, ty));
            match (ok, err) {
                (Some(ok), Some(err)) => format!("result<{ok}, {err}>"),
                (Some(ok), None) => format!("result<{ok}>"),
                (None, Some(err)) => format!("result<_, {err}>"),
                (None, None) => "result".into(),
            }
        }
        Type::Tuple(types) => {
            let types = types
                .iter()
                .map(|ty| wit_type_name(wit, ty))
                .collect::<Vec<_>>()
                .join(", ");
            format!("tuple<{types}>")
        }
        Type::List(ty) => {
            format!("list<{}>", wit_type_name(wit, ty))
        }
    }
}

fn rust_type_name(module: &str, ty: &Type) -> String {
    match ty {
        Type::Bool => "bool".into(),
        Type::U8 => "u8".into(),
        Type::S8 => "i8".into(),
        Type::U16 => "u16".into(),
        Type::S16 => "i16".into(),
        Type::U32 => "u32".into(),
        Type::S32 => "i32".into(),
        Type::U64 => "u64".into(),
        Type::S64 => "i64".into(),
        Type::Float32 => "f32".into(),
        Type::Float64 => "f64".into(),
        Type::Char => "char".into(),
        Type::String => "String".into(),
        Type::Record { id, .. } => {
            format!(
                "{module}::Record{id}Type{}",
                if borrows(ty) { "Result" } else { "" }
            )
        }
        Type::Variant { id, .. } => {
            format!(
                "{module}::Variant{id}Type{}",
                if borrows(ty) { "Result" } else { "" }
            )
        }
        Type::Flags { id, .. } => {
            format!("{module}::Flags{id}Type")
        }
        Type::Enum { id, .. } => {
            format!("{module}::Enum{id}Type")
        }
        Type::Union { id, .. } => {
            format!(
                "{module}::Union{id}Type{}",
                if borrows(ty) { "Result" } else { "" }
            )
        }
        Type::Option(ty) => {
            format!("Option<{}>", rust_type_name(module, ty))
        }
        Type::Result { ok, err } => {
            let ok = ok
                .as_ref()
                .map(|ty| rust_type_name(module, ty))
                .unwrap_or_else(|| "()".to_owned());
            let err = err
                .as_ref()
                .map(|ty| rust_type_name(module, ty))
                .unwrap_or_else(|| "()".to_owned());
            format!("Result<{ok}, {err}>")
        }
        Type::Tuple(types) => {
            let types = types
                .iter()
                .map(|ty| format!("{},", rust_type_name(module, ty)))
                .collect::<Vec<_>>()
                .join(" ");
            format!("({types})")
        }
        Type::List(ty) => {
            format!("Vec<{}>", rust_type_name(module, ty))
        }
    }
}

fn equality(a: &str, b: &str, ty: &Type) -> String {
    match ty {
        Type::Bool
        | Type::U8
        | Type::S8
        | Type::U16
        | Type::S16
        | Type::U32
        | Type::S32
        | Type::U64
        | Type::S64
        | Type::Char
        | Type::String
        | Type::Flags { .. }
        | Type::Enum { .. } => format!("({a} == {b})"),
        Type::Float32 | Type::Float64 => format!("(({a}.is_nan() && {b}.is_nan()) || {a} == {b})"),
        Type::Record { fields, .. } => {
            if fields.is_empty() {
                "true".into()
            } else {
                fields
                    .iter()
                    .enumerate()
                    .map(|(index, ty)| {
                        equality(&format!("{a}.f{index}"), &format!("{b}.f{index}"), ty)
                    })
                    .collect::<Vec<_>>()
                    .join(" && ")
            }
        }
        Type::Variant { id, cases } => {
            assert!(!cases.is_empty());
            let name = format!(
                "exports::Variant{id}Type{}",
                if borrows(ty) { "Result" } else { "" }
            );
            let cases = cases
                .iter()
                .enumerate()
                .map(|(index, ty)| {
                    if let Some(ty) = ty {
                        let test = equality("a", "b", ty);
                        format!("({name}::C{index}(a), {name}::C{index}(b)) => {{ {test} }}")
                    } else {
                        format!("({name}::C{index}, {name}::C{index}) => {{ true }}")
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!("(match (&{a}, &{b}) {{ {cases} _ => false }})")
        }
        Type::Union { id, cases } => {
            assert!(!cases.is_empty());
            let name = format!(
                "exports::Union{id}Type{}",
                if borrows(ty) { "Result" } else { "" }
            );
            let cases = union_case_names(cases)
                .iter()
                .zip(cases)
                .map(|(case_name, ty)| {
                    let test = equality("a", "b", ty);
                    format!("({name}::{case_name}(a), {name}::{case_name}(b)) => {{ {test} }}")
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!("(match (&{a}, &{b}) {{ {cases} _ => false }})")
        }
        Type::Option(ty) => {
            let test = equality("a", "b", ty);
            format!("(match (&{a}, &{b}) {{ (Some(a), Some(b)) => {test}, (None, None) => true, _ => false }})")
        }
        Type::Result { ok, err } => {
            let ok = ok
                .as_ref()
                .map(|ty| equality("a", "b", ty))
                .unwrap_or_else(|| "true".to_owned());
            let err = err
                .as_ref()
                .map(|ty| equality("a", "b", ty))
                .unwrap_or_else(|| "true".to_owned());
            format!("(match (&{a}, &{b}) {{ (Ok(a), Ok(b)) => {ok}, (Err(a), Err(b)) => {err}, _ => false }})")
        }
        Type::Tuple(types) => {
            if types.is_empty() {
                "true".into()
            } else {
                types
                    .iter()
                    .enumerate()
                    .map(|(index, ty)| {
                        equality(&format!("{a}.{index}"), &format!("{b}.{index}"), ty)
                    })
                    .collect::<Vec<_>>()
                    .join(" && ")
            }
        }
        Type::List(ty) => format!(
            "{a}.len() == {b}.len() && {a}.iter().zip({b}.iter()).all(|(a, b)| {})",
            equality("a", "b", ty)
        ),
    }
}

fn strategy(ty: &Type, max_list_size: usize) -> String {
    match ty {
        Type::Bool => "proptest::bool::ANY".into(),
        Type::U8 => "proptest::num::u8::ANY".into(),
        Type::S8 => "proptest::num::i8::ANY".into(),
        Type::U16 => "proptest::num::u16::ANY".into(),
        Type::S16 => "proptest::num::i16::ANY".into(),
        Type::U32 => "proptest::num::u32::ANY".into(),
        Type::S32 => "proptest::num::i32::ANY".into(),
        Type::U64 => "proptest::num::u64::ANY".into(),
        Type::S64 => "proptest::num::i64::ANY".into(),
        Type::Float32 => "proptest::num::f32::ANY".into(),
        Type::Float64 => "proptest::num::f64::ANY".into(),
        Type::Char => "proptest::char::any()".into(),
        Type::String => r#"proptest::string::string_regex(".*").unwrap()"#.into(),
        Type::Record { id, fields } => {
            if fields.is_empty() {
                format!("Just(exports::Record{id}Type {{}})")
            } else {
                let strategies = fields
                    .iter()
                    .map(|ty| strategy(ty, max_list_size))
                    .collect::<Vec<_>>();

                let params = (0..strategies.len())
                    .map(|index| format!("f{index},"))
                    .collect::<Vec<_>>()
                    .join(" ");

                let strategies = strategies
                    .iter()
                    .map(|s| format!("{s},"))
                    .collect::<Vec<_>>()
                    .join(" ");

                let inits = (0..fields.len())
                    .map(|index| format!("f{index}"))
                    .collect::<Vec<_>>()
                    .join(", ");

                format!(
                    "({strategies}).prop_map(|({params})| \
                     exports::Record{id}Type{} {{ {inits} }})",
                    if borrows(ty) { "Result" } else { "" }
                )
            }
        }
        Type::Variant { id, cases } => {
            assert!(!cases.is_empty());
            let name = format!(
                "exports::Variant{id}Type{}",
                if borrows(ty) { "Result" } else { "" }
            );
            let length = cases.len();
            let cases = cases
                .iter()
                .enumerate()
                .map(|(index, ty)| {
                    if let Some(ty) = ty {
                        let strategy = strategy(ty, max_list_size);
                        format!("index => {strategy}.prop_map({name}::C{index}).boxed()")
                    } else {
                        format!("index => Just({name}::C{index}).boxed()")
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("(0..{length}).prop_flat_map(move |index| match index {{ {cases}, _ => unreachable!() }})")
        }
        Type::Flags { id, count } => {
            let name = format!("exports::Flags{id}Type");

            let flags = (0..*count)
                .map(|index| {
                    format!(" | if v[{index}] {{ {name}::F{index} }} else {{ {name}::empty() }}")
                })
                .collect::<Vec<_>>()
                .concat();

            format!(
                "proptest::collection::vec(proptest::bool::ANY, {count})\
                 .prop_map(|v| {name}::empty(){flags})"
            )
        }
        Type::Enum { id, count } => {
            let name = format!("exports::Enum{id}Type");
            let cases = (0..*count)
                .map(|index| format!("index => {name}::C{index}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("(0..{count}).prop_map(move |index| match index {{ {cases}, _ => unreachable!() }})")
        }
        Type::Union { id, cases } => {
            assert!(!cases.is_empty());
            let name = format!(
                "exports::Union{id}Type{}",
                if borrows(ty) { "Result" } else { "" }
            );
            let length = cases.len();
            let cases = union_case_names(cases)
                .iter()
                .zip(cases)
                .map(|(case_name, ty)| {
                    let strategy = strategy(ty, max_list_size);
                    format!("index => {strategy}.prop_map({name}::{case_name}).boxed()")
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("(0..{length}).prop_flat_map(move |index| match index {{ {cases}, _ => unreachable!() }})")
        }
        Type::Option(ty) => {
            format!("proptest::option::of({})", strategy(ty, max_list_size))
        }
        Type::Result { ok, err } => {
            let ok = ok
                .as_ref()
                .map(|ty| strategy(ty, max_list_size))
                .unwrap_or_else(|| "Just(())".to_owned());
            let err = err
                .as_ref()
                .map(|ty| strategy(ty, max_list_size))
                .unwrap_or_else(|| "Just(())".to_owned());
            format!("proptest::result::maybe_err({ok}, {err})")
        }
        Type::Tuple(types) => {
            if types.is_empty() {
                "Just(())".into()
            } else {
                format!(
                    "({})",
                    types
                        .iter()
                        .map(|ty| format!("{},", strategy(ty, max_list_size)))
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            }
        }
        Type::List(ty) => {
            format!(
                "proptest::collection::vec({}, 0..{max_list_size}.max(1))",
                strategy(ty, max_list_size / 2)
            )
        }
    }
}

pub fn generate() -> Result<()> {
    let seed = if let Ok(seed) = env::var("COMPONENTIZE_PY_TEST_SEED") {
        hex::decode(seed)?
    } else {
        let mut seed = vec![0u8; 32];
        getrandom::getrandom(&mut seed)?;
        seed
    };

    println!(
        "cargo:warning=using seed {} (set COMPONENTIZE_PY_TEST_SEED env var to override)",
        hex::encode(&seed)
    );
    println!("cargo:rerun-if-env-changed=COMPONENTIZE_PY_TEST_SEED");
    println!(
        "cargo:rustc-env=COMPONENTIZE_PY_TEST_SEED={}",
        hex::encode(&seed)
    );

    let count = if let Ok(count) = env::var("COMPONENTIZE_PY_TEST_COUNT") {
        usize::from_str(&count)?
    } else {
        DEFAULT_TEST_COUNT
    };

    println!(
        "cargo:warning=using count {count} (set COMPONENTIZE_PY_TEST_COUNT env var to override)",
    );
    println!("cargo:rerun-if-env-changed=COMPONENTIZE_PY_TEST_COUNT");
    println!("cargo:rustc-env=COMPONENTIZE_PY_TEST_COUNT={count}",);

    let config = Config::default();
    let algorithm = config.rng_algorithm;
    let mut runner = TestRunner::new_with_rng(config, TestRng::from_seed(algorithm, &seed));
    let param_strategy = proptest::collection::vec(
        any_type(MAX_TUPLE_SIZE, Rc::new(Cell::new(0))),
        1..MAX_PARAM_COUNT,
    );
    let mut wit = String::new();
    let mut host_functions = String::new();
    let mut guest_functions = String::new();
    let mut test_functions = String::new();
    let mut typed_function_fields = String::new();
    let mut typed_function_inits = String::new();

    for test_index in 0..count {
        let params = param_strategy
            .new_tree(&mut runner)
            .map_err(|reason| anyhow!("unable to generate params: {reason:?}"))?
            .current();

        assert!(!params.is_empty());

        // WIT type and function declarations
        {
            let types = params
                .iter()
                .map(|ty| wit_type_name(&mut wit, ty))
                .collect::<Vec<_>>();

            let result = match types.len() {
                0 => String::new(),
                1 => format!(" -> {}", types[0]),
                _ => format!(" -> tuple<{}>", types.join(", ")),
            };

            let params = types
                .iter()
                .enumerate()
                .map(|(index, name)| format!("v{index}: {name}"))
                .collect::<Vec<_>>()
                .join(", ");

            writeln!(&mut wit, "\n    echo{test_index}: func({params}){result}").unwrap();
        }

        // Guest function implementations
        {
            let params = (0..params.len())
                .map(|index| format!("v{index}"))
                .collect::<Vec<_>>()
                .join(", ");

            write!(
                &mut guest_functions,
                "    def echo{test_index}({params}):
        return imports.echo{test_index}({params})
"
            )
            .unwrap();
        }

        // Host function implementations
        {
            let types = params
                .iter()
                .map(|ty| rust_type_name("imports", ty))
                .collect::<Vec<_>>();

            let result_type = match types.len() {
                0 => "()".to_owned(),
                1 => types[0].clone(),
                _ => format!("({})", types.join(", ")),
            };

            let params = types
                .iter()
                .enumerate()
                .map(|(index, name)| format!("v{index}: {name}"))
                .collect::<Vec<_>>()
                .join(", ");

            let result = match types.len() {
                0 => "()".to_owned(),
                1 => "v0".to_owned(),
                _ => format!(
                    "({})",
                    (0..types.len())
                        .map(|index| format!("v{index}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            };

            writeln!(
                &mut host_functions,
                "async fn echo{test_index}(&mut self, {params}) -> Result<{result_type}> {{ Ok({result}) }}"
            )
                .unwrap();
        }

        // Typed function fields and inits
        {
            let types = params
                .iter()
                .map(|ty| rust_type_name("exports", ty))
                .collect::<Vec<_>>();

            let result_type = match types.len() {
                0 => "()".to_owned(),
                1 => types[0].clone(),
                _ => format!("({})", types.join(", ")),
            };

            let params = types
                .iter()
                .map(|ty| format!("{ty},"))
                .collect::<Vec<_>>()
                .join(" ");

            writeln!(
                &mut typed_function_fields,
                "echo{test_index}: TypedFunc<({params}), ({result_type},)>,"
            )
            .unwrap();

            writeln!(
                &mut typed_function_inits,
                r#"echo{test_index}: instance.typed_func::<({params}), ({result_type},)>("echo{test_index}")?,"#
            )
            .unwrap();
        }

        // Test function implementations
        {
            let types = params
                .iter()
                .map(|ty| rust_type_name("exports", ty))
                .collect::<Vec<_>>();

            let args = (0..params.len())
                .map(|index| format!("v.0.{index},"))
                .collect::<Vec<_>>()
                .join(" ");

            let equality = params
                .iter()
                .enumerate()
                .map(|(index, ty)| {
                    equality(&format!("self.0.{index}"), &format!("other.0.{index}"), ty)
                })
                .collect::<Vec<_>>()
                .join(" && ");

            let strategies = params
                .iter()
                .map(|ty| format!("{},", strategy(ty, MAX_LIST_SIZE)))
                .collect::<Vec<_>>()
                .join(" ");

            let types = types
                .iter()
                .map(|ty| format!("{ty},"))
                .collect::<Vec<_>>()
                .join(" ");

            let mut call = format!(
                "runtime.block_on(instance.echo{test_index}.call_async(&mut *store, ({args})))?"
            );

            if params.len() == 1 {
                call = format!("({call},)");
            }

            write!(
                &mut test_functions,
                r#"
#[derive(Clone, Debug)]
struct TestType{test_index}(({types}));

impl TestType{test_index} {{
    fn strategy() -> impl Strategy<Value = Self> {{
        ({strategies}).prop_map(Self)
    }}
}}

impl PartialEq<TestType{test_index}> for TestType{test_index} {{
    fn eq(&self, other: &Self) -> bool {{
        {equality}
    }}
}}

#[test]
fn test{test_index}() -> Result<()> {{
    TESTER.all_eq(&TestType{test_index}::strategy(), |v, instance, store, runtime| {{
        let result = {call}.0;
        runtime.block_on(instance.echo{test_index}.post_return_async(store))?;
        Ok(TestType{test_index}(result))
    }})
}}
"#
            )
            .unwrap();
        }
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());

    let wit = format!(
        "\
interface foo {{
    {wit}
}}

default world echoes-generated {{
    import imports: self.foo
    export exports: self.foo
}}
"
    );

    let wit_path = out_dir.join("echoes-generated.wit");
    fs::write(&wit_path, wit.as_bytes())?;

    let rust = format!(
        r##"
use {{
    super::{{Tester, SEED}},
    anyhow::Result,
    async_trait::async_trait,
    once_cell::sync::Lazy,
    proptest::strategy::{{Just, Strategy}},
    wasi_preview2::WasiCtx,
    wasmtime::{{
        component::{{InstancePre, Linker, TypedFunc}},
        Store,
    }},
}};

wasmtime::component::bindgen!({{
    path: {wit_path:?},
    world: "echoes-generated",
    async: true
}});

pub struct Exports {{
   {typed_function_fields}
}}

pub struct Host {{
    wasi: WasiCtx,
}}

#[async_trait]
impl imports::Host for Host {{
    {host_functions}
}}

#[async_trait]
impl super::Host for Host {{
    type World = Exports;

    fn new(wasi: WasiCtx) -> Self {{
        Self {{ wasi }}
    }}

    fn add_to_linker(linker: &mut Linker<Self>) -> Result<()> {{
        wasi_host::command::add_to_linker(&mut *linker, |host| &mut host.wasi)?;
        imports::add_to_linker(linker, |host| host)?;
        Ok(())
    }}

    async fn instantiate_pre(
        store: &mut Store<Self>,
        pre: &InstancePre<Self>,
    ) -> Result<Self::World> {{
        let instance = pre.instantiate_async(&mut *store).await?;
        let mut exports = instance.exports(&mut *store);
        let mut instance = exports.instance("exports").unwrap();
        Ok(Self::World {{
           {typed_function_inits}
        }})
    }}
}}

const GUEST_CODE: &str = r#"
from echoes_generated import exports
from echoes_generated.imports import imports

class Exports(exports.Exports):
{guest_functions}
"#;

static TESTER: Lazy<Tester<Host>> = Lazy::new(|| {{
    Tester::<Host>::new(include_str!({wit_path:?}), GUEST_CODE, *SEED).unwrap()
}});

{test_functions}
"##
    );

    fs::write(out_dir.join("echoes_generated.rs"), rust.as_bytes())?;

    _ = Command::new("rustfmt")
        .arg("--edition")
        .arg("2021")
        .arg(out_dir.join("echoes_generated.rs"))
        .status();

    Ok(())
}

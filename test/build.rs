use {
    anyhow::{anyhow, Result},
    proptest::{
        strategy::{Just, Strategy, ValueTree},
        test_runner::{Config, TestRng, TestRunner},
    },
    std::{env, fmt::Write, fs, path::PathBuf, process::Command, str::FromStr},
};

const DEFAULT_TEST_COUNT: usize = 10;
const MAX_PARAM_COUNT: usize = 8;
const MAX_SIZE: usize = 20;
const MAX_TUPLE_SIZE: usize = 12;

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
    Record(Vec<Type>),
    Tuple(Vec<Type>),
    List(Box<Type>),
}

fn any_type(max_size: usize) -> impl Strategy<Value = Type> {
    (0..16).prop_flat_map(move |index| match index {
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
        13 => proptest::collection::vec(any_type(max_size / 2), 0..max_size.max(1))
            .prop_map(Type::Record)
            .boxed(),
        14 => proptest::collection::vec(
            any_type(max_size / 2),
            0..max_size.max(2).min(MAX_TUPLE_SIZE),
        )
        .prop_map(Type::Tuple)
        .boxed(),
        15 => any_type(max_size)
            .prop_map(|ty| Type::List(Box::new(ty)))
            .boxed(),
        _ => unreachable!(),
    })
}

fn wit_type_name(wit: &mut String, test_index: usize, ty: &Type, ty_index: &mut usize) -> String {
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
        Type::Record(types) => {
            let types = types
                .iter()
                .enumerate()
                .map(|(index, ty)| {
                    let ty = wit_type_name(wit, test_index, ty, ty_index);
                    format!("f{index}: {ty}")
                })
                .collect::<Vec<_>>()
                .join(",\n        ");

            write!(
                wit,
                "
    record record-test{test_index}-type{ty_index} {{
        {types}
    }}
"
            )
            .unwrap();

            let name = format!("record-test{test_index}-type{ty_index}");
            *ty_index += 1;
            name
        }
        Type::Tuple(types) => {
            let types = types
                .iter()
                .map(|ty| wit_type_name(wit, test_index, ty, ty_index))
                .collect::<Vec<_>>()
                .join(", ");
            format!("tuple<{types}>")
        }
        Type::List(ty) => {
            format!("list<{}>", wit_type_name(wit, test_index, ty, ty_index))
        }
    }
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
        | Type::Char => false,
        Type::String | Type::List(_) => true,
        Type::Record(types) | Type::Tuple(types) => types.iter().any(borrows),
    }
}

fn rust_type_name(module: &str, test_index: usize, ty: &Type, ty_index: &mut usize) -> String {
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
        Type::Record(types) => {
            for ty in types {
                rust_type_name(module, test_index, ty, ty_index);
            }
            let name = format!(
                "{module}::RecordTest{test_index}Type{ty_index}{}",
                if borrows(ty) { "Result" } else { "" }
            );
            *ty_index += 1;
            name
        }
        Type::Tuple(types) => {
            let types = types
                .iter()
                .map(|ty| format!("{},", rust_type_name(module, test_index, ty, ty_index)))
                .collect::<Vec<_>>()
                .join(" ");
            format!("({types})")
        }
        Type::List(ty) => {
            format!("Vec<{}>", rust_type_name(module, test_index, ty, ty_index))
        }
    }
}

fn test_arg(
    temporaries: &mut String,
    base: &str,
    test_index: usize,
    ty: &Type,
    ty_index: &mut usize,
    tmp_index: &mut usize,
) -> String {
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
        | Type::Char => base.to_owned(),
        Type::String => format!("{base}.as_str()"),
        Type::Record(types) => {
            let inits = types
                .iter()
                .enumerate()
                .map(|(index, ty)| {
                    format!(
                        "f{index}: {}",
                        test_arg(
                            temporaries,
                            &format!("{base}.f{index}"),
                            test_index,
                            ty,
                            ty_index,
                            tmp_index
                        )
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");

            let arg = format!(
                "exports::RecordTest{test_index}Type{ty_index}{} {{ {inits} }}",
                if borrows(ty) { "Param" } else { "" }
            );
            *ty_index += 1;
            arg
        }
        Type::Tuple(types) => {
            let args = types
                .iter()
                .enumerate()
                .map(|(index, ty)| {
                    format!(
                        "{},",
                        test_arg(
                            temporaries,
                            &format!("{base}.{index}"),
                            test_index,
                            ty,
                            ty_index,
                            tmp_index
                        )
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!("({args})")
        }
        Type::List(ty) => {
            if borrows(ty) {
                let tmp = format!("tmp{tmp_index}");
                *tmp_index += 1;

                let arg = test_arg(temporaries, "v", test_index, ty, ty_index, tmp_index);
                writeln!(
                    temporaries,
                    "let {tmp} = {base}.iter().map(|v| {arg}).collect::<Vec<_>>();"
                )
                .unwrap();

                format!("{tmp}.as_slice()")
            } else {
                test_arg(temporaries, "v", test_index, ty, ty_index, tmp_index);
                format!("{base}.as_slice()")
            }
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
        | Type::String => format!("({a} == {b})"),
        Type::Float32 | Type::Float64 => format!("(({a}.is_nan() && {b}.is_nan()) || {a} == {b})"),
        Type::Record(types) => {
            if types.is_empty() {
                "true".into()
            } else {
                types
                    .iter()
                    .enumerate()
                    .map(|(index, ty)| {
                        equality(&format!("{a}.f{index}"), &format!("{b}.f{index}"), ty)
                    })
                    .collect::<Vec<_>>()
                    .join(" && ")
            }
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

fn strategy(test_index: usize, ty: &Type, ty_index: &mut usize, max_size: usize) -> String {
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
        Type::Record(types) => {
            let strategy = if types.is_empty() {
                format!("Just(exports::RecordTest{test_index}Type{ty_index} {{}})")
            } else {
                let strategies = types
                    .iter()
                    .map(|ty| strategy(test_index, ty, ty_index, max_size))
                    .collect::<Vec<_>>();

                let (strategies, params) = match (strategies.len() - 1) / 10 {
                    0 => (
                        strategies
                            .iter()
                            .map(|s| format!("{s},"))
                            .collect::<Vec<_>>()
                            .join(" "),
                        (0..strategies.len())
                            .map(|index| format!("f{index},"))
                            .collect::<Vec<_>>()
                            .join(" "),
                    ),
                    1..=10 => (
                        strategies
                            .chunks(10)
                            .map(|s| {
                                format!(
                                    "({}),",
                                    s.iter()
                                        .map(|s| format!("{s},"))
                                        .collect::<Vec<_>>()
                                        .join(" ")
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(" "),
                        {
                            let mut index = 0;
                            strategies
                                .chunks(10)
                                .map(|s| {
                                    format!(
                                        "({}),",
                                        s.iter()
                                            .map(|_| {
                                                let param = format!("f{index},");
                                                index += 1;
                                                param
                                            })
                                            .collect::<Vec<_>>()
                                            .join(" ")
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join(" ")
                        },
                    ),
                    _ => unreachable!("expected MAX_SIZE <= 109, was {MAX_SIZE}"),
                };

                let inits = (0..types.len())
                    .map(|index| format!("f{index}"))
                    .collect::<Vec<_>>()
                    .join(", ");

                format!(
                    "({strategies}).prop_map(|({params})| \
                     exports::RecordTest{test_index}Type{ty_index}{} {{ {inits} }})",
                    if borrows(ty) { "Result" } else { "" }
                )
            };
            *ty_index += 1;
            strategy
        }
        Type::Tuple(types) => {
            if types.is_empty() {
                "Just(())".into()
            } else {
                format!(
                    "({})",
                    types
                        .iter()
                        .map(|ty| format!("{},", strategy(test_index, ty, ty_index, max_size)))
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            }
        }
        Type::List(ty) => {
            format!(
                "proptest::collection::vec({}, 0..{max_size}.max(1))",
                strategy(test_index, ty, ty_index, max_size / 2)
            )
        }
    }
}

fn main() -> Result<()> {
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
    let param_strategy = proptest::collection::vec(any_type(MAX_SIZE), 1..MAX_PARAM_COUNT);
    let mut wit = String::new();
    let mut host_functions = String::new();
    let mut guest_functions = String::new();
    let mut test_functions = String::new();

    for test_index in 0..count {
        let params = param_strategy
            .new_tree(&mut runner)
            .map_err(|reason| anyhow!("unable to generate params: {reason:?}"))?
            .current();

        assert!(!params.is_empty());

        // WIT type and function declarations
        {
            let types = {
                let mut ty_index = 0;
                params
                    .iter()
                    .map(|ty| wit_type_name(&mut wit, test_index, ty, &mut ty_index))
                    .collect::<Vec<_>>()
            };

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
                "\
def exports_echo{test_index}({params}):
    return imports.echo{test_index}({params})
"
            )
            .unwrap();
        }

        // Host function implementations
        {
            let types = {
                let mut ty_index = 0;
                params
                    .iter()
                    .map(|ty| rust_type_name("imports", test_index, ty, &mut ty_index))
                    .collect::<Vec<_>>()
            };

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

        // Test function implementations
        {
            let types = {
                let mut ty_index = 0;
                params
                    .iter()
                    .map(|ty| rust_type_name("exports", test_index, ty, &mut ty_index))
                    .collect::<Vec<_>>()
            };

            let mut temporaries = String::new();

            let args = {
                let mut ty_index = 0;
                let mut tmp_index = 0;
                params
                    .iter()
                    .enumerate()
                    .map(|(index, ty)| {
                        test_arg(
                            &mut temporaries,
                            &format!("v.0.{index}"),
                            test_index,
                            ty,
                            &mut ty_index,
                            &mut tmp_index,
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            };

            let equality = params
                .iter()
                .enumerate()
                .map(|(index, ty)| {
                    equality(&format!("self.0.{index}"), &format!("other.0.{index}"), ty)
                })
                .collect::<Vec<_>>()
                .join(" && ");

            let strategies = {
                let mut ty_index = 0;
                params
                    .iter()
                    .map(|ty| format!("{},", strategy(test_index, ty, &mut ty_index, MAX_SIZE)))
                    .collect::<Vec<_>>()
                    .join(" ")
            };

            let types = types
                .iter()
                .map(|ty| format!("{ty},"))
                .collect::<Vec<_>>()
                .join(" ");

            let mut call = format!(
                "runtime.block_on(instance.exports().call_echo{test_index}(store, {args}))?"
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
        {temporaries}
        Ok(TestType{test_index}({call}))
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
    crate::tests::{{self, Tester, SEED}},
    anyhow::Result,
    async_trait::async_trait,
    once_cell::sync::Lazy,
    proptest::strategy::{{Just, Strategy}},
    wasi_preview2::WasiCtx,
    wasmtime::{{
        component::{{InstancePre, Linker}},
        Store,
    }},
}};

wasmtime::component::bindgen!({{
    path: {wit_path:?},
    world: "echoes-generated",
    async: true
}});

pub struct Host {{
    wasi: WasiCtx,
}}

#[async_trait]
impl imports::Host for Host {{
    {host_functions}
}}

#[async_trait]
impl tests::Host for Host {{
    type World = EchoesGenerated;

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
        Ok(EchoesGenerated::instantiate_pre(store, pre).await?.0)
    }}
}}

const GUEST_CODE: &str = r#"
import imports

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

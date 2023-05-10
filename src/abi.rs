use {
    std::iter,
    wasm_encoder::ValType,
    wit_parser::{FlagsRepr, Resolve, Type, TypeDefKind},
};

pub const MAX_FLAT_PARAMS: usize = 16;
pub const MAX_FLAT_RESULTS: usize = 1;

pub fn align(a: usize, b: usize) -> usize {
    assert!(b.is_power_of_two());
    (a + (b - 1)) & !(b - 1)
}

pub struct Abi {
    pub size: usize,
    pub align: usize,
    pub flattened: Vec<ValType>,
}

pub fn record_abi(resolve: &Resolve, types: impl IntoIterator<Item = Type>) -> Abi {
    let mut size = 0_usize;
    let mut align_ = 1;
    let mut flattened = Vec::new();
    for ty in types {
        let abi = abi(resolve, ty);
        size = align(size, abi.align);
        size += abi.size;
        if abi.align > align_ {
            align_ = abi.align;
        }
        flattened.extend(abi.flattened);
    }

    Abi {
        size: align(size, align_),
        align: align_,
        flattened,
    }
}

pub fn record_abi_limit(
    resolve: &Resolve,
    types: impl IntoIterator<Item = Type>,
    limit: usize,
) -> Abi {
    let mut abi = record_abi(resolve, types);
    if abi.flattened.len() > limit {
        abi.flattened = vec![ValType::I32];
    }
    abi
}

fn join(a: ValType, b: ValType) -> ValType {
    if a == b {
        a
    } else if let (ValType::I32, ValType::F32) | (ValType::F32, ValType::I32) = (a, b) {
        ValType::I32
    } else {
        ValType::I64
    }
}

pub fn discriminant_size(count: usize) -> usize {
    match count {
        1..=0xFF => 1,
        0x100..=0xFFFF => 2,
        0x1_0000..=0xFFFF_FFFF => 4,
        _ => unreachable!(),
    }
}

fn variant_abi(resolve: &Resolve, types: impl IntoIterator<Item = Option<Type>>) -> Abi {
    let mut size = 0_usize;
    let mut align_ = 1;
    let mut flattened = Vec::new();
    let mut count = 0;
    for ty in types {
        count += 1;
        if let Some(ty) = ty {
            let abi = abi(resolve, ty);
            if abi.size > size {
                size = abi.size;
            }
            if abi.align > align_ {
                align_ = abi.align;
            }
            for (index, ty) in abi.flattened.iter().enumerate() {
                if index == flattened.len() {
                    flattened.push(*ty);
                } else {
                    flattened[index] = join(flattened[index], *ty);
                }
            }
        }
    }

    let discriminant_size = discriminant_size(count);
    let align_ = align_.max(discriminant_size);
    let size = align(size + align(discriminant_size, align_), align_);
    let flattened = iter::once(ValType::I32).chain(flattened).collect();

    Abi {
        size,
        align: align_,
        flattened,
    }
}

pub fn has_pointer(resolve: &Resolve, ty: Type) -> bool {
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
        | Type::Float64 => false,
        Type::String => true,
        Type::Id(id) => match &resolve.types[id].kind {
            TypeDefKind::Record(record) => record
                .fields
                .iter()
                .any(|field| has_pointer(resolve, field.ty)),
            TypeDefKind::Variant(variant) => variant
                .cases
                .iter()
                .any(|case| case.ty.map(|ty| has_pointer(resolve, ty)).unwrap_or(false)),
            TypeDefKind::Enum(_) | TypeDefKind::Flags(_) => false,
            TypeDefKind::Union(un) => un.cases.iter().any(|case| has_pointer(resolve, case.ty)),
            TypeDefKind::Option(ty) => has_pointer(resolve, *ty),
            TypeDefKind::Result(result) => {
                result
                    .ok
                    .map(|ty| has_pointer(resolve, ty))
                    .unwrap_or(false)
                    || result
                        .err
                        .map(|ty| has_pointer(resolve, ty))
                        .unwrap_or(false)
            }
            TypeDefKind::Tuple(tuple) => tuple.types.iter().any(|ty| has_pointer(resolve, *ty)),
            TypeDefKind::List(_) => true,
            TypeDefKind::Type(ty) => has_pointer(resolve, *ty),
            kind => todo!("{kind:?}"),
        },
    }
}

pub fn abi(resolve: &Resolve, ty: Type) -> Abi {
    match ty {
        Type::Bool | Type::U8 | Type::S8 => Abi {
            size: 1,
            align: 1,
            flattened: vec![ValType::I32],
        },
        Type::U16 | Type::S16 => Abi {
            size: 2,
            align: 2,
            flattened: vec![ValType::I32],
        },
        Type::U32 | Type::S32 | Type::Char => Abi {
            size: 4,
            align: 4,
            flattened: vec![ValType::I32],
        },
        Type::U64 | Type::S64 => Abi {
            size: 8,
            align: 8,
            flattened: vec![ValType::I64],
        },
        Type::Float32 => Abi {
            size: 4,
            align: 4,
            flattened: vec![ValType::F32],
        },
        Type::Float64 => Abi {
            size: 8,
            align: 8,
            flattened: vec![ValType::F64],
        },
        Type::String => Abi {
            size: 8,
            align: 4,
            flattened: vec![ValType::I32; 2],
        },
        Type::Id(id) => match &resolve.types[id].kind {
            TypeDefKind::Record(record) => {
                record_abi(resolve, record.fields.iter().map(|field| field.ty))
            }
            TypeDefKind::Variant(variant) => {
                variant_abi(resolve, variant.cases.iter().map(|case| case.ty))
            }
            TypeDefKind::Enum(en) => variant_abi(resolve, en.cases.iter().map(|_| None)),
            TypeDefKind::Union(un) => {
                variant_abi(resolve, un.cases.iter().map(|case| Some(case.ty)))
            }
            TypeDefKind::Option(ty) => variant_abi(resolve, [None, Some(*ty)]),
            TypeDefKind::Result(result) => variant_abi(resolve, [result.ok, result.err]),
            TypeDefKind::Flags(flags) => {
                let repr = flags.repr();

                Abi {
                    size: match &repr {
                        FlagsRepr::U8 => 1,
                        FlagsRepr::U16 => 2,
                        FlagsRepr::U32(count) => 4 * *count,
                    },
                    align: match &repr {
                        FlagsRepr::U8 | FlagsRepr::U32(0) => 1,
                        FlagsRepr::U16 => 2,
                        FlagsRepr::U32(_) => 4,
                    },
                    flattened: vec![ValType::I32; repr.count()],
                }
            }
            TypeDefKind::Tuple(tuple) => record_abi(resolve, tuple.types.iter().copied()),
            TypeDefKind::List(_) => Abi {
                size: 8,
                align: 4,
                flattened: vec![ValType::I32; 2],
            },
            TypeDefKind::Type(ty) => abi(resolve, *ty),
            kind => todo!("{kind:?}"),
        },
    }
}

pub fn is_option(resolve: &Resolve, ty: Type) -> bool {
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

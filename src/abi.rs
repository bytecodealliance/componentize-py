use {
    std::{iter, ops::Deref},
    wasm_encoder::ValType,
    wit_parser::{Resolve, Results, Type, TypeDefKind},
};

pub(crate) const MAX_FLAT_PARAMS: usize = 16;
pub(crate) const MAX_FLAT_RESULTS: usize = 1;

pub(crate) trait Types {
    fn types(&self) -> Box<dyn Iterator<Item = Type>>;
}

impl Types for &[(String, Type)] {
    fn types(&self) -> Box<dyn Iterator<Item = Type>> {
        Box::new(
            self.iter()
                .map(|(_, ty)| *ty)
                .collect::<Vec<_>>()
                .into_iter(),
        )
    }
}

impl Types for Results {
    fn types(&self) -> Box<dyn Iterator<Item = Type>> {
        match self {
            Self::Named(params) => params.deref().types(),
            Self::Anon(ty) => Box::new(iter::once(*ty)),
        }
    }
}

pub(crate) fn align(a: usize, b: usize) -> usize {
    assert!(b.is_power_of_two());
    (a + (b - 1)) & !(b - 1)
}

pub(crate) struct Abi {
    pub(crate) size: usize,
    pub(crate) align: usize,
    pub(crate) flattened: Vec<ValType>,
}

pub(crate) fn record_abi(resolve: &Resolve, types: impl IntoIterator<Item = Type>) -> Abi {
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
        size,
        align: align_,
        flattened,
    }
}

pub(crate) fn record_abi_limit(
    resolve: &Resolve,
    types: impl IntoIterator<Item = Type>,
    limit: usize,
) -> Abi {
    let mut abi = record_abi(resolve, types);
    abi.flattened.truncate(limit);
    abi
}

pub(crate) fn abi(resolve: &Resolve, ty: Type) -> Abi {
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
            TypeDefKind::List(_) => Abi {
                size: 8,
                align: 4,
                flattened: vec![ValType::I32; 2],
            },
            _ => todo!(),
        },
    }
}

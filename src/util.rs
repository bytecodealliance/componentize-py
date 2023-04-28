use {
    std::{iter, ops::Deref},
    wit_parser::{Flags, FlagsRepr, Results, Type},
};

pub trait Types {
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

impl Types for Flags {
    fn types(&self) -> Box<dyn Iterator<Item = Type>> {
        match self.repr() {
            FlagsRepr::U8 => Box::new(iter::once(Type::U8)),
            FlagsRepr::U16 => Box::new(iter::once(Type::U16)),
            FlagsRepr::U32(count) => Box::new(iter::repeat(Type::U32).take(count)),
        }
    }
}

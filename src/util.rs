use {
    std::iter,
    wit_parser::{Flags, FlagsRepr, Param, Type},
};

pub trait Types {
    fn types(&self) -> Box<dyn Iterator<Item = Type>>;
}

impl Types for &[Param] {
    fn types(&self) -> Box<dyn Iterator<Item = Type>> {
        Box::new(self.iter().map(|p| p.ty).collect::<Vec<_>>().into_iter())
    }
}

impl Types for Option<Type> {
    fn types(&self) -> Box<dyn Iterator<Item = Type>> {
        Box::new((*self).into_iter())
    }
}

impl Types for Flags {
    fn types(&self) -> Box<dyn Iterator<Item = Type>> {
        match self.repr() {
            FlagsRepr::U8 => Box::new(iter::once(Type::U8)),
            FlagsRepr::U16 => Box::new(iter::once(Type::U16)),
            FlagsRepr::U32(count) => Box::new(std::iter::repeat_n(Type::U32, count)),
        }
    }
}

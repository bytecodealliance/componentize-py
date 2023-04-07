use {
    std::{iter, ops::Deref},
    wit_parser::{Results, Type},
};

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

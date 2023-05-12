use {
    crate::{
        abi::{self, Abi, MAX_FLAT_PARAMS, MAX_FLAT_RESULTS},
        summary::{MyFunction, Summary},
        util::Types as _,
    },
    componentize_py_shared::ReturnStyle,
    indexmap::IndexSet,
    std::collections::HashMap,
    wasm_encoder::{BlockType, Instruction as Ins, MemArg, ValType},
    wit_parser::{Resolve, Results, Type, TypeDefKind, TypeId},
};

// Assume Wasm32
// TODO: Wasm64 support
const WORD_SIZE: usize = 4;
const WORD_ALIGN: usize = 2; // as a power of two

const STACK_ALIGNMENT: usize = 8;

pub const DISPATCHABLE_CORE_PARAM_COUNT: usize = 3;
pub const DISPATCH_CORE_PARAM_COUNT: usize = DISPATCHABLE_CORE_PARAM_COUNT + 1;

const DISCRIMINANT_FIELD_INDEX: i32 = 0;
const PAYLOAD_FIELD_INDEX: i32 = 1;

macro_rules! declare_enum {
    ($name:ident { $( $variant:ident ),* } $list:ident) => {
        #[derive(Debug, Copy, Clone, Hash, PartialEq, Eq)]
        pub enum $name {
            $( $variant ),*
        }

        pub static $list: &[$name] = &[$( $name::$variant ),*];
    }
}

declare_enum! {
    Link {
        Dispatch,
        Free,
        LowerI32,
        LowerI64,
        LowerF32,
        LowerF64,
        LowerChar,
        LowerString,
        GetField,
        GetListLength,
        GetListElement,
        Allocate,
        LiftI32,
        LiftI64,
        LiftF32,
        LiftF64,
        LiftChar,
        LiftString,
        Init,
        MakeList,
        ListAppend,
        None,
        GetBytes,
        MakeBytes
    } LINK_LIST
}

pub fn mem_arg(offset: u64, align: u32) -> MemArg {
    MemArg {
        offset,
        align,
        memory_index: 0,
    }
}

pub struct FunctionBindgen<'a> {
    pub local_types: Vec<ValType>,
    pub instructions: Vec<Ins<'static>>,
    resolve: &'a Resolve,
    stack_pointer: u32,
    link_map: &'a HashMap<Link, u32>,
    types: &'a IndexSet<TypeId>,
    params: &'a [(String, Type)],
    results: &'a Results,
    params_abi: Abi,
    results_abi: Abi,
    local_stack: Vec<bool>,
    param_count: usize,
    tuple_types: &'a HashMap<usize, TypeId>,
    option_type: Option<TypeId>,
    nesting_option_type: Option<TypeId>,
    result_type: Option<TypeId>,
}

impl<'a> FunctionBindgen<'a> {
    pub fn new(
        summary: &'a Summary,
        function: &'a MyFunction,
        stack_pointer: u32,
        link_map: &'a HashMap<Link, u32>,
    ) -> Self {
        Self {
            resolve: summary.resolve,
            stack_pointer,
            link_map,
            types: &summary.types,
            params: function.params,
            results: function.results,
            params_abi: abi::record_abi(summary.resolve, function.params.types()),
            results_abi: abi::record_abi(summary.resolve, function.results.types()),
            local_types: Vec::new(),
            local_stack: Vec::new(),
            instructions: Vec::new(),
            param_count: function.core_export_type(summary.resolve).0.len(),
            tuple_types: &summary.tuple_types,
            option_type: summary.option_type,
            nesting_option_type: summary.nesting_option_type,
            result_type: summary.result_type,
        }
    }

    pub fn compile_import(&mut self, index: u32) {
        // Arg 0: *const Python
        let context = 0;
        // Arg 1: *const &PyAny
        let input = 1;
        // Arg 2: *mut &PyAny
        let output = 2;

        let locals = if self.params_abi.flattened.len() <= MAX_FLAT_PARAMS {
            let locals = self
                .params_abi
                .flattened
                .clone()
                .iter()
                .map(|ty| self.push_local(*ty))
                .collect::<Vec<_>>();

            let mut lift_index = 0;
            let mut load_offset = 0;
            for ty in self.params.types() {
                let abi = abi::abi(self.resolve, ty);

                let value = self.push_local(ValType::I32);

                self.push(Ins::LocalGet(input));
                self.push(Ins::I32Load(mem_arg(
                    load_offset,
                    WORD_ALIGN.try_into().unwrap(),
                )));
                self.push(Ins::LocalSet(value));

                self.lower(ty, context, value);

                for local in locals[lift_index..][..abi.flattened.len()].iter().rev() {
                    self.push(Ins::LocalSet(*local));
                }

                for local in &locals[lift_index..][..abi.flattened.len()] {
                    self.push(Ins::LocalGet(*local));
                }

                lift_index += abi.flattened.len();
                load_offset += u64::try_from(WORD_SIZE).unwrap();

                self.pop_local(value, ValType::I32);
            }

            Some(locals)
        } else {
            self.push_stack(self.params_abi.size);

            let mut store_offset = 0;
            let mut load_offset = 0;
            for ty in self.params.types() {
                let value = self.push_local(ValType::I32);
                let destination = self.push_local(ValType::I32);

                let abi = abi::abi(self.resolve, ty);
                store_offset = abi::align(store_offset, abi.align);

                self.get_stack();
                self.push(Ins::I32Const(store_offset.try_into().unwrap()));
                self.push(Ins::I32Add);
                self.push(Ins::LocalSet(destination));

                self.push(Ins::LocalGet(input));
                self.push(Ins::I32Load(mem_arg(
                    load_offset,
                    WORD_ALIGN.try_into().unwrap(),
                )));
                self.push(Ins::LocalSet(value));

                self.store(ty, context, value, destination);

                store_offset += abi.size;
                load_offset += u64::try_from(WORD_SIZE).unwrap();

                self.pop_local(destination, ValType::I32);
                self.pop_local(value, ValType::I32);
            }

            self.get_stack();

            None
        };

        if self.results_abi.flattened.len() > MAX_FLAT_RESULTS {
            self.push_stack(self.results_abi.size);

            self.get_stack();
        }

        self.push(Ins::Call(index));

        if self.results_abi.flattened.len() <= MAX_FLAT_RESULTS {
            let locals = self
                .results_abi
                .flattened
                .clone()
                .iter()
                .map(|ty| {
                    let local = self.push_local(*ty);
                    self.push(Ins::LocalSet(local));
                    local
                })
                .collect::<Vec<_>>();

            self.lift_record(self.results.types(), context, &locals, output);

            for (local, ty) in locals.iter().zip(&self.results_abi.flattened.clone()).rev() {
                self.pop_local(*local, *ty);
            }
        } else {
            let source = self.push_local(ValType::I32);

            self.get_stack();
            self.push(Ins::LocalSet(source));

            self.load_record(self.results.types(), context, source, output);

            self.pop_local(source, ValType::I32);
            self.pop_stack(self.results_abi.size);
        }

        if let Some(locals) = locals {
            self.free_lowered_record(self.params.types(), &locals);

            for (local, ty) in locals.iter().zip(&self.params_abi.flattened.clone()).rev() {
                self.pop_local(*local, *ty);
            }
        } else {
            let value = self.push_local(ValType::I32);

            self.get_stack();
            self.push(Ins::LocalSet(value));

            self.free_stored_record(self.params.types(), value);

            self.pop_local(value, ValType::I32);
            self.pop_stack(self.params_abi.size);
        }
    }

    pub fn compile_export(&mut self, index: i32, lift: i32, lower: i32) {
        let return_style = match self.results.types().collect::<Vec<_>>().as_slice() {
            [Type::Id(id)] if matches!(&self.resolve.types[*id].kind, TypeDefKind::Result(_)) => {
                ReturnStyle::Result
            }
            _ => ReturnStyle::Normal,
        };

        self.push(Ins::I32Const(index));
        self.push(Ins::I32Const(lift));
        self.push(Ins::I32Const(lower));
        self.push(Ins::I32Const(
            self.params.types().count().try_into().unwrap(),
        ));
        self.push(Ins::I32Const(return_style as _));

        if self.params_abi.flattened.len() <= MAX_FLAT_PARAMS {
            self.push_stack(self.params_abi.size);

            let destination = self.push_local(ValType::I32);
            self.get_stack();
            self.push(Ins::LocalSet(destination));

            self.store_copy_record(
                self.params.types(),
                &(0..self.params_abi.flattened.len().try_into().unwrap()).collect::<Vec<_>>(),
                destination,
            );

            self.pop_local(destination, ValType::I32);

            self.get_stack();
        } else {
            self.push(Ins::LocalGet(0));
        };

        let result = if self.results_abi.flattened.len() <= MAX_FLAT_RESULTS {
            self.push_stack(self.results_abi.size);
            self.get_stack();

            None
        } else {
            let result = self.push_local(ValType::I32);
            self.push(Ins::I32Const(self.results_abi.size.try_into().unwrap()));
            self.push(Ins::I32Const(self.results_abi.align.try_into().unwrap()));
            self.link_call(Link::Allocate);
            self.push(Ins::LocalTee(result));

            Some(result)
        };

        self.link_call(Link::Dispatch);

        if let Some(result) = result {
            self.push(Ins::LocalGet(result));
            self.pop_local(result, ValType::I32);
        } else {
            let source = self.push_local(ValType::I32);
            self.get_stack();
            self.push(Ins::LocalSet(source));

            self.load_copy_record(self.results.types(), source);

            self.pop_local(source, ValType::I32);

            self.pop_stack(self.results_abi.size);
        }

        if self.params_abi.flattened.len() <= MAX_FLAT_PARAMS {
            self.pop_stack(self.params_abi.size);
        }
    }

    pub fn compile_export_lift(&mut self) {
        // Arg 0: *const Python
        let context = 0;
        // Arg 1: *const MyParams
        let source = 1;
        // Arg 2: *mut &PyAny
        let destination = 2;

        self.load_record(self.params.types(), context, source, destination);
    }

    pub fn compile_export_lower(&mut self) {
        // Arg 0: *const Python
        let context = 0;
        // Arg 1: *const &PyAny
        let source = 1;
        // Arg 2: *mut MyResults
        let destination = 2;

        let mut store_offset = 0;
        let mut load_offset = 0;
        for ty in self.results.types() {
            let abi = abi::abi(self.resolve, ty);
            store_offset = abi::align(store_offset, abi.align);

            let field_value = self.push_local(ValType::I32);
            let field_destination = self.push_local(ValType::I32);

            self.push(Ins::LocalGet(source));
            self.push(Ins::I32Load(mem_arg(
                load_offset,
                WORD_ALIGN.try_into().unwrap(),
            )));
            self.push(Ins::LocalSet(field_value));

            self.push(Ins::LocalGet(destination));
            self.push(Ins::I32Const(store_offset.try_into().unwrap()));
            self.push(Ins::I32Add);
            self.push(Ins::LocalSet(field_destination));

            self.store(ty, context, field_value, field_destination);

            store_offset += abi.size;
            load_offset += u64::try_from(WORD_SIZE).unwrap();

            self.pop_local(field_destination, ValType::I32);
            self.pop_local(field_value, ValType::I32);
        }
    }

    pub fn compile_export_post_return(&mut self) {
        if self.results_abi.flattened.len() > MAX_FLAT_RESULTS {
            // Arg 0: *mut MyResults
            let value = 0;

            self.free_stored_record(self.results.types(), value);

            self.push(Ins::LocalGet(value));
            self.push(Ins::I32Const(self.results_abi.size.try_into().unwrap()));
            self.push(Ins::I32Const(self.results_abi.align.try_into().unwrap()));
            self.link_call(Link::Free);
        } else {
            unreachable!()
        }
    }

    fn push_stack(&mut self, size: usize) {
        self.push(Ins::GlobalGet(self.stack_pointer));
        self.push(Ins::I32Const(
            abi::align(size, STACK_ALIGNMENT).try_into().unwrap(),
        ));
        self.push(Ins::I32Sub);
        self.push(Ins::GlobalSet(self.stack_pointer));
    }

    fn pop_stack(&mut self, size: usize) {
        self.push(Ins::GlobalGet(self.stack_pointer));
        self.push(Ins::I32Const(
            abi::align(size, STACK_ALIGNMENT).try_into().unwrap(),
        ));
        self.push(Ins::I32Add);
        self.push(Ins::GlobalSet(self.stack_pointer));
    }

    fn push(&mut self, instruction: Ins<'static>) {
        self.instructions.push(instruction)
    }

    fn link_call(&mut self, link: Link) {
        self.push(Ins::Call(*self.link_map.get(&link).unwrap()));
    }

    fn get_stack(&mut self) {
        self.push(Ins::GlobalGet(self.stack_pointer));
    }

    fn push_local(&mut self, ty: ValType) -> u32 {
        while self.local_types.len() > self.local_stack.len()
            && self.local_types[self.local_stack.len()] != ty
        {
            self.local_stack.push(false);
        }

        self.local_stack.push(true);
        if self.local_types.len() < self.local_stack.len() {
            self.local_types.push(ty);
        }

        (self.param_count + self.local_stack.len() - 1)
            .try_into()
            .unwrap()
    }

    fn pop_local(&mut self, index: u32, ty: ValType) {
        assert!(
            index
                == (self.param_count + self.local_stack.len() - 1)
                    .try_into()
                    .unwrap()
        );
        assert!(ty == self.local_types[self.local_stack.len() - 1]);

        self.local_stack.pop();
        while let Some(false) = self.local_stack.last() {
            self.local_stack.pop();
        }
    }

    fn lower(&mut self, ty: Type, context: u32, value: u32) {
        match ty {
            Type::Bool | Type::U8 | Type::U16 | Type::U32 | Type::S8 | Type::S16 | Type::S32 => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value));
                self.link_call(Link::LowerI32);
            }
            Type::U64 | Type::S64 => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value));
                self.link_call(Link::LowerI64);
            }
            Type::Float32 => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value));
                self.link_call(Link::LowerF32);
            }
            Type::Float64 => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value));
                self.link_call(Link::LowerF64);
            }
            Type::Char => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value));
                self.link_call(Link::LowerChar);
            }
            Type::String => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value));
                self.push_stack(WORD_SIZE * 2);
                self.get_stack();
                self.link_call(Link::LowerString);
                self.get_stack();
                self.push(Ins::I32Load(mem_arg(0, WORD_ALIGN.try_into().unwrap())));
                self.get_stack();
                self.push(Ins::I32Load(mem_arg(
                    WORD_SIZE.try_into().unwrap(),
                    WORD_ALIGN.try_into().unwrap(),
                )));
                self.pop_stack(WORD_SIZE * 2);
            }
            Type::Id(id) => match &self.resolve.types[id].kind {
                TypeDefKind::Record(record) => {
                    self.lower_record(id, record.fields.iter().map(|f| f.ty), context, value);
                }
                TypeDefKind::Variant(variant) => {
                    self.lower_variant(
                        id,
                        &abi::abi(self.resolve, ty),
                        variant.cases.iter().map(|c| c.ty),
                        context,
                        value,
                    );
                }
                TypeDefKind::Enum(en) => {
                    self.lower_variant(
                        id,
                        &abi::abi(self.resolve, ty),
                        en.cases.iter().map(|_| None),
                        context,
                        value,
                    );
                }
                TypeDefKind::Union(un) => {
                    self.lower_variant(
                        id,
                        &abi::abi(self.resolve, ty),
                        un.cases.iter().map(|c| Some(c.ty)),
                        context,
                        value,
                    );
                }
                TypeDefKind::Option(some) => {
                    self.lower_variant(
                        self.get_option_type(*some),
                        &abi::abi(self.resolve, ty),
                        [None, Some(*some)],
                        context,
                        value,
                    );
                }
                TypeDefKind::Result(result) => {
                    self.lower_variant(
                        self.result_type.unwrap(),
                        &abi::abi(self.resolve, ty),
                        [result.ok, result.err],
                        context,
                        value,
                    );
                }
                TypeDefKind::Flags(flags) => {
                    self.lower_record(id, flags.types(), context, value);
                }
                TypeDefKind::Tuple(tuple) => {
                    self.lower_record(
                        *self.tuple_types.get(&tuple.types.len()).unwrap(),
                        tuple.types.iter().copied(),
                        context,
                        value,
                    );
                }
                TypeDefKind::List(ty) => {
                    let abi = abi::abi(self.resolve, *ty);

                    let length = self.push_local(ValType::I32);
                    let destination = self.push_local(ValType::I32);

                    self.push(Ins::LocalGet(context));
                    self.push(Ins::LocalGet(value));
                    self.link_call(Link::GetListLength);
                    self.push(Ins::LocalSet(length));

                    self.push(Ins::LocalGet(length));
                    self.push(Ins::I32Const(abi.size.try_into().unwrap()));
                    self.push(Ins::I32Mul);
                    self.push(Ins::I32Const(abi.align.try_into().unwrap()));
                    self.link_call(Link::Allocate);
                    self.push(Ins::LocalSet(destination));

                    if let Type::U8 | Type::S8 = ty {
                        self.push(Ins::LocalGet(context));
                        self.push(Ins::LocalGet(value));
                        self.push(Ins::LocalGet(destination));
                        self.push(Ins::LocalGet(length));
                        self.link_call(Link::GetBytes);
                    } else {
                        let index = self.push_local(ValType::I32);
                        let element_value = self.push_local(ValType::I32);
                        let element_destination = self.push_local(ValType::I32);

                        self.push(Ins::I32Const(0));
                        self.push(Ins::LocalSet(index));

                        self.push(Ins::Loop(BlockType::Empty));

                        self.push(Ins::LocalGet(index));
                        self.push(Ins::LocalGet(length));
                        self.push(Ins::I32Ne);

                        self.push(Ins::If(BlockType::Empty));

                        self.push(Ins::LocalGet(context));
                        self.push(Ins::LocalGet(value));
                        self.push(Ins::LocalGet(index));
                        self.link_call(Link::GetListElement);
                        self.push(Ins::LocalSet(element_value));

                        self.push(Ins::LocalGet(destination));
                        self.push(Ins::LocalGet(index));
                        self.push(Ins::I32Const(abi.size.try_into().unwrap()));
                        self.push(Ins::I32Mul);
                        self.push(Ins::I32Add);
                        self.push(Ins::LocalSet(element_destination));

                        self.store(*ty, context, element_value, element_destination);

                        self.push(Ins::LocalGet(index));
                        self.push(Ins::I32Const(1));
                        self.push(Ins::I32Add);
                        self.push(Ins::LocalSet(index));

                        self.push(Ins::Br(1));

                        self.push(Ins::End);

                        self.push(Ins::End);

                        self.pop_local(element_destination, ValType::I32);
                        self.pop_local(element_value, ValType::I32);
                        self.pop_local(index, ValType::I32);
                    }

                    self.push(Ins::LocalGet(destination));
                    self.push(Ins::LocalGet(length));

                    self.pop_local(destination, ValType::I32);
                    self.pop_local(length, ValType::I32);
                }
                TypeDefKind::Type(ty) => self.lower(*ty, context, value),
                kind => todo!("{kind:?}"),
            },
        }
    }

    fn lower_record(
        &mut self,
        id: TypeId,
        types: impl IntoIterator<Item = Type>,
        context: u32,
        value: u32,
    ) {
        let type_index = self.types.get_index_of(&id).unwrap();
        for (field_index, ty) in types.into_iter().enumerate() {
            let field_value = self.push_local(ValType::I32);

            self.push(Ins::LocalGet(context));
            self.push(Ins::LocalGet(value));
            self.push(Ins::I32Const(type_index.try_into().unwrap()));
            self.push(Ins::I32Const(field_index.try_into().unwrap()));
            self.link_call(Link::GetField);
            self.push(Ins::LocalSet(field_value));

            self.lower(ty, context, field_value);

            self.pop_local(field_value, ValType::I32);
        }
    }

    fn lower_variant(
        &mut self,
        id: TypeId,
        abi: &Abi,
        types: impl IntoIterator<Item = Option<Type>>,
        context: u32,
        value: u32,
    ) {
        // TODO: instead of storing to and then loading from memory, lower directly to the primary stack (and/or
        // locals)

        let destination = self.push_local(ValType::I32);
        self.push_stack(abi.size);
        self.get_stack();
        self.push(Ins::LocalSet(destination));

        let types = types.into_iter().collect::<Vec<_>>();

        self.store_variant(id, abi, types.clone(), context, value, destination);

        self.load_copy_variant(abi, types, destination);

        self.pop_stack(abi.size);
        self.pop_local(destination, ValType::I32);
    }

    fn store(&mut self, ty: Type, context: u32, value: u32, destination: u32) {
        match ty {
            Type::Bool | Type::U8 | Type::S8 => {
                self.push(Ins::LocalGet(destination));
                self.lower(ty, context, value);
                self.push(Ins::I32Store8(mem_arg(0, 0)));
            }
            Type::U16 | Type::S16 => {
                self.push(Ins::LocalGet(destination));
                self.lower(ty, context, value);
                self.push(Ins::I32Store16(mem_arg(0, 1)));
            }
            Type::U32 | Type::S32 => {
                self.push(Ins::LocalGet(destination));
                self.lower(ty, context, value);
                self.push(Ins::I32Store(mem_arg(0, 2)));
            }
            Type::U64 | Type::S64 => {
                self.push(Ins::LocalGet(destination));
                self.lower(ty, context, value);
                self.push(Ins::I64Store(mem_arg(0, 3)));
            }
            Type::Float32 => {
                self.push(Ins::LocalGet(destination));
                self.lower(ty, context, value);
                self.push(Ins::F32Store(mem_arg(0, 2)));
            }
            Type::Float64 => {
                self.push(Ins::LocalGet(destination));
                self.lower(ty, context, value);
                self.push(Ins::F64Store(mem_arg(0, 3)));
            }
            Type::Char => {
                self.push(Ins::LocalGet(destination));
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value));
                self.link_call(Link::LowerChar);
                self.push(Ins::I32Store(mem_arg(0, 2)));
            }
            Type::String => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value));
                self.push(Ins::LocalGet(destination));
                self.link_call(Link::LowerString);
            }
            Type::Id(id) => match &self.resolve.types[id].kind {
                TypeDefKind::Record(record) => {
                    self.store_record(
                        id,
                        record.fields.iter().map(|f| f.ty),
                        context,
                        value,
                        destination,
                    );
                }
                TypeDefKind::Variant(variant) => {
                    self.store_variant(
                        id,
                        &abi::abi(self.resolve, ty),
                        variant.cases.iter().map(|c| c.ty),
                        context,
                        value,
                        destination,
                    );
                }
                TypeDefKind::Enum(en) => {
                    self.store_variant(
                        id,
                        &abi::abi(self.resolve, ty),
                        en.cases.iter().map(|_| None),
                        context,
                        value,
                        destination,
                    );
                }
                TypeDefKind::Union(un) => {
                    self.store_variant(
                        id,
                        &abi::abi(self.resolve, ty),
                        un.cases.iter().map(|c| Some(c.ty)),
                        context,
                        value,
                        destination,
                    );
                }
                TypeDefKind::Option(some) => {
                    self.store_variant(
                        self.get_option_type(*some),
                        &abi::abi(self.resolve, ty),
                        [None, Some(*some)],
                        context,
                        value,
                        destination,
                    );
                }
                TypeDefKind::Result(result) => {
                    self.store_variant(
                        self.result_type.unwrap(),
                        &abi::abi(self.resolve, ty),
                        [result.ok, result.err],
                        context,
                        value,
                        destination,
                    );
                }
                TypeDefKind::Flags(flags) => {
                    self.store_record(id, flags.types(), context, value, destination);
                }
                TypeDefKind::Tuple(tuple) => {
                    self.store_record(
                        *self.tuple_types.get(&tuple.types.len()).unwrap(),
                        tuple.types.iter().copied(),
                        context,
                        value,
                        destination,
                    );
                }
                TypeDefKind::List(_) => {
                    let length = self.push_local(ValType::I32);

                    self.push(Ins::LocalGet(destination));
                    self.lower(ty, context, value);
                    self.push(Ins::LocalSet(length));
                    self.push(Ins::I32Store(mem_arg(0, WORD_ALIGN.try_into().unwrap())));
                    self.push(Ins::LocalGet(destination));
                    self.push(Ins::LocalGet(length));
                    self.push(Ins::I32Store(mem_arg(
                        WORD_SIZE.try_into().unwrap(),
                        WORD_ALIGN.try_into().unwrap(),
                    )));

                    self.pop_local(length, ValType::I32);
                }
                TypeDefKind::Type(ty) => self.store(*ty, context, value, destination),
                kind => todo!("{kind:?}"),
            },
        }
    }

    fn store_record(
        &mut self,
        id: TypeId,
        types: impl IntoIterator<Item = Type>,
        context: u32,
        value: u32,
        destination: u32,
    ) {
        let type_index = self.types.get_index_of(&id).unwrap();
        let mut store_offset = 0;
        for (field_index, ty) in types.into_iter().enumerate() {
            let abi = abi::abi(self.resolve, ty);
            store_offset = abi::align(store_offset, abi.align);

            let field_value = self.push_local(ValType::I32);
            let field_destination = self.push_local(ValType::I32);

            self.push(Ins::LocalGet(context));
            self.push(Ins::LocalGet(value));
            self.push(Ins::I32Const(type_index.try_into().unwrap()));
            self.push(Ins::I32Const(field_index.try_into().unwrap()));
            self.link_call(Link::GetField);
            self.push(Ins::LocalSet(field_value));

            self.push(Ins::LocalGet(destination));
            self.push(Ins::I32Const(store_offset.try_into().unwrap()));
            self.push(Ins::I32Add);
            self.push(Ins::LocalSet(field_destination));

            self.store(ty, context, field_value, field_destination);

            store_offset += abi.size;

            self.pop_local(field_destination, ValType::I32);
            self.pop_local(field_value, ValType::I32);
        }
    }

    fn search_variant(
        &mut self,
        block_type: BlockType,
        types: &[Option<Type>],
        discriminant: u32,
        predicate: impl (Fn(&Self, Option<Type>) -> bool) + Copy,
        fun: impl Fn(&mut Self, Option<Type>) + Copy,
    ) {
        match types {
            [] => unreachable!(),
            [ty] => fun(self, *ty),
            types => {
                if types.iter().any(|ty| predicate(self, *ty)) {
                    let middle = types.len() / 2;
                    self.push(Ins::LocalGet(discriminant));
                    self.push(Ins::I32Const(middle.try_into().unwrap()));
                    self.push(Ins::I32LtU);
                    self.push(Ins::If(block_type));
                    self.search_variant(block_type, &types[..middle], discriminant, predicate, fun);
                    self.push(Ins::Else);
                    self.search_variant(block_type, &types[middle..], discriminant, predicate, fun);
                    self.push(Ins::End);
                } else {
                    fun(self, None);
                }
            }
        }
    }

    fn store_variant(
        &mut self,
        id: TypeId,
        abi: &Abi,
        types: impl IntoIterator<Item = Option<Type>>,
        context: u32,
        value: u32,
        destination: u32,
    ) {
        let type_index = self.types.get_index_of(&id).unwrap();
        let types = types.into_iter().collect::<Vec<_>>();
        let discriminant_size = abi::discriminant_size(types.len());
        let discriminant = self.push_local(ValType::I32);

        self.push(Ins::LocalGet(context));
        self.push(Ins::LocalGet(context));
        self.push(Ins::LocalGet(value));
        self.push(Ins::I32Const(type_index.try_into().unwrap()));
        self.push(Ins::I32Const(DISCRIMINANT_FIELD_INDEX));
        self.link_call(Link::GetField);
        self.link_call(Link::LowerI32);
        self.push(Ins::LocalSet(discriminant));

        self.push(Ins::LocalGet(destination));
        self.push(Ins::LocalGet(discriminant));
        match discriminant_size {
            1 => self.push(Ins::I32Store8(mem_arg(0, 0))),
            2 => self.push(Ins::I32Store16(mem_arg(0, 1))),
            4 => self.push(Ins::I32Store(mem_arg(0, 2))),
            _ => unreachable!(),
        }

        if types.iter().any(Option::is_some) {
            let payload = self.push_local(ValType::I32);
            let payload_destination = self.push_local(ValType::I32);

            self.push(Ins::LocalGet(context));
            self.push(Ins::LocalGet(value));
            self.push(Ins::I32Const(type_index.try_into().unwrap()));
            self.push(Ins::I32Const(PAYLOAD_FIELD_INDEX));
            self.link_call(Link::GetField);
            self.push(Ins::LocalSet(payload));

            self.push(Ins::LocalGet(destination));
            self.push(Ins::I32Const(
                abi::align(discriminant_size, abi.align).try_into().unwrap(),
            ));
            self.push(Ins::I32Add);
            self.push(Ins::LocalSet(payload_destination));

            self.search_variant(
                BlockType::Empty,
                &types,
                discriminant,
                |_, ty| ty.is_some(),
                |this, ty| {
                    if let Some(ty) = ty {
                        this.store(ty, context, payload, payload_destination);
                    }
                },
            );

            self.pop_local(payload_destination, ValType::I32);
            self.pop_local(payload, ValType::I32);
        }

        self.pop_local(discriminant, ValType::I32);
    }

    fn store_copy(&mut self, ty: Type, source: &[u32], destination: u32) {
        match ty {
            Type::Bool | Type::U8 | Type::S8 => {
                self.push(Ins::LocalGet(destination));
                self.push(Ins::LocalGet(source[0]));
                self.push(Ins::I32Store8(mem_arg(0, 0)));
            }
            Type::U16 | Type::S16 => {
                self.push(Ins::LocalGet(destination));
                self.push(Ins::LocalGet(source[0]));
                self.push(Ins::I32Store16(mem_arg(0, 1)));
            }
            Type::U32 | Type::S32 | Type::Char => {
                self.push(Ins::LocalGet(destination));
                self.push(Ins::LocalGet(source[0]));
                self.push(Ins::I32Store(mem_arg(0, 2)));
            }
            Type::U64 | Type::S64 => {
                self.push(Ins::LocalGet(destination));
                self.push(Ins::LocalGet(source[0]));
                self.push(Ins::I64Store(mem_arg(0, 3)));
            }
            Type::Float32 => {
                self.push(Ins::LocalGet(destination));
                self.push(Ins::LocalGet(source[0]));
                self.push(Ins::F32Store(mem_arg(0, 2)));
            }
            Type::Float64 => {
                self.push(Ins::LocalGet(destination));
                self.push(Ins::LocalGet(source[0]));
                self.push(Ins::F64Store(mem_arg(0, 3)));
            }
            Type::String => {
                self.push(Ins::LocalGet(destination));
                self.push(Ins::LocalGet(source[0]));
                self.push(Ins::I32Store(mem_arg(0, WORD_ALIGN.try_into().unwrap())));
                self.push(Ins::LocalGet(destination));
                self.push(Ins::LocalGet(source[1]));
                self.push(Ins::I32Store(mem_arg(
                    WORD_SIZE.try_into().unwrap(),
                    WORD_ALIGN.try_into().unwrap(),
                )));
            }
            Type::Id(id) => match &self.resolve.types[id].kind {
                TypeDefKind::Record(record) => {
                    self.store_copy_record(record.fields.iter().map(|f| f.ty), source, destination);
                }
                TypeDefKind::Variant(variant) => {
                    self.store_copy_variant(
                        &abi::abi(self.resolve, ty),
                        variant.cases.iter().map(|c| c.ty),
                        source,
                        destination,
                    );
                }
                TypeDefKind::Enum(en) => {
                    self.store_copy_variant(
                        &abi::abi(self.resolve, ty),
                        en.cases.iter().map(|_| None),
                        source,
                        destination,
                    );
                }
                TypeDefKind::Union(un) => {
                    self.store_copy_variant(
                        &abi::abi(self.resolve, ty),
                        un.cases.iter().map(|c| Some(c.ty)),
                        source,
                        destination,
                    );
                }
                TypeDefKind::Option(some) => {
                    self.store_copy_variant(
                        &abi::abi(self.resolve, ty),
                        [None, Some(*some)],
                        source,
                        destination,
                    );
                }
                TypeDefKind::Result(result) => {
                    self.store_copy_variant(
                        &abi::abi(self.resolve, ty),
                        [result.ok, result.err],
                        source,
                        destination,
                    );
                }
                TypeDefKind::Flags(flags) => {
                    self.store_copy_record(flags.types(), source, destination);
                }
                TypeDefKind::Tuple(tuple) => {
                    self.store_copy_record(tuple.types.iter().copied(), source, destination);
                }
                TypeDefKind::List(_) => {
                    self.push(Ins::LocalGet(destination));
                    self.push(Ins::LocalGet(source[0]));
                    self.push(Ins::I32Store(mem_arg(0, WORD_ALIGN.try_into().unwrap())));
                    self.push(Ins::LocalGet(destination));
                    self.push(Ins::LocalGet(source[1]));
                    self.push(Ins::I32Store(mem_arg(
                        WORD_SIZE.try_into().unwrap(),
                        WORD_ALIGN.try_into().unwrap(),
                    )));
                }
                TypeDefKind::Type(ty) => self.store_copy(*ty, source, destination),
                kind => todo!("{kind:?}"),
            },
        }
    }

    fn store_copy_record(
        &mut self,
        types: impl IntoIterator<Item = Type>,
        source: &[u32],
        destination: u32,
    ) {
        let mut local_index = 0;
        let mut store_offset = 0;
        for ty in types {
            let abi = abi::abi(self.resolve, ty);
            store_offset = abi::align(store_offset, abi.align);

            let field_destination = self.push_local(ValType::I32);

            self.push(Ins::LocalGet(destination));
            self.push(Ins::I32Const(store_offset.try_into().unwrap()));
            self.push(Ins::I32Add);
            self.push(Ins::LocalSet(field_destination));

            self.store_copy(
                ty,
                &source[local_index..][..abi.flattened.len()],
                field_destination,
            );

            local_index += abi.flattened.len();
            store_offset += abi.size;

            self.pop_local(field_destination, ValType::I32);
        }
    }

    fn convert(&mut self, source_type: ValType, destination_type: ValType) {
        match (source_type, destination_type) {
            (ValType::I32, ValType::I64) => self.push(Ins::I64ExtendI32U),
            (ValType::I64, ValType::I32) => self.push(Ins::I32WrapI64),
            (ValType::I32, ValType::F32) => self.push(Ins::F32ReinterpretI32),
            (ValType::F32, ValType::I32) => self.push(Ins::I32ReinterpretF32),
            (ValType::I64, ValType::F64) => self.push(Ins::F64ReinterpretI64),
            (ValType::F64, ValType::I64) => self.push(Ins::I64ReinterpretF64),
            (ValType::F32, ValType::I64) => {
                self.push(Ins::I32ReinterpretF32);
                self.push(Ins::I64ExtendI32U);
            }
            (ValType::I64, ValType::F32) => {
                self.push(Ins::I32WrapI64);
                self.push(Ins::F32ReinterpretI32);
            }
            _ => unreachable!("can't convert {source_type:?} to {destination_type:?}"),
        }
    }

    fn convert_all(
        &mut self,
        abi: &Abi,
        payload_type: Type,
        value: &[u32],
    ) -> (Vec<u32>, Vec<(u32, ValType)>) {
        let payload_abi = abi::abi(self.resolve, payload_type);
        let mut my_value = Vec::new();
        let locals = payload_abi
            .flattened
            .iter()
            .zip(abi.flattened.iter().skip(1))
            .zip(value)
            .filter_map(|((payload_type, joined_type), value)| {
                if payload_type == joined_type {
                    my_value.push(*value);
                    None
                } else {
                    let local = self.push_local(*payload_type);
                    self.push(Ins::LocalGet(*value));
                    self.convert(*joined_type, *payload_type);
                    self.push(Ins::LocalSet(local));
                    my_value.push(local);
                    Some((local, *payload_type))
                }
            })
            .collect::<Vec<_>>();

        (my_value, locals)
    }

    fn store_copy_variant(
        &mut self,
        abi: &Abi,
        types: impl IntoIterator<Item = Option<Type>>,
        source: &[u32],
        destination: u32,
    ) {
        let types = types.into_iter().collect::<Vec<_>>();
        let discriminant_size = abi::discriminant_size(types.len());

        self.push(Ins::LocalGet(destination));
        self.push(Ins::LocalGet(source[0]));
        match discriminant_size {
            1 => self.push(Ins::I32Store8(mem_arg(0, 0))),
            2 => self.push(Ins::I32Store16(mem_arg(0, 1))),
            4 => self.push(Ins::I32Store(mem_arg(0, 2))),
            _ => unreachable!(),
        }

        if types.iter().any(Option::is_some) {
            let payload_destination = self.push_local(ValType::I32);

            self.push(Ins::LocalGet(destination));
            self.push(Ins::I32Const(
                abi::align(discriminant_size, abi.align).try_into().unwrap(),
            ));
            self.push(Ins::I32Add);
            self.push(Ins::LocalSet(payload_destination));

            self.search_variant(
                BlockType::Empty,
                &types,
                source[0],
                |_, ty| ty.is_some(),
                |this, ty| {
                    if let Some(ty) = ty {
                        let (source, locals) = this.convert_all(abi, ty, &source[1..]);

                        this.store_copy(ty, &source, payload_destination);

                        for (local, ty) in locals.into_iter().rev() {
                            this.pop_local(local, ty);
                        }
                    }
                },
            );

            self.pop_local(payload_destination, ValType::I32);
        }
    }

    fn lift(&mut self, ty: Type, context: u32, value: &[u32]) {
        match ty {
            Type::Bool | Type::U8 | Type::U16 | Type::U32 | Type::S8 | Type::S16 | Type::S32 => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value[0]));
                self.link_call(Link::LiftI32);
            }
            Type::U64 | Type::S64 => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value[0]));
                self.link_call(Link::LiftI64);
            }
            Type::Float32 => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value[0]));
                self.link_call(Link::LiftF32);
            }
            Type::Float64 => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value[0]));
                self.link_call(Link::LiftF64);
            }
            Type::Char => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value[0]));
                self.link_call(Link::LiftChar);
            }
            Type::String => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(value[0]));
                self.push(Ins::LocalGet(value[1]));
                self.link_call(Link::LiftString);
            }
            Type::Id(id) => match &self.resolve.types[id].kind {
                TypeDefKind::Record(record) => {
                    self.lift_record_onto_stack(
                        id,
                        record.fields.iter().map(|f| f.ty),
                        context,
                        value,
                    );
                }
                TypeDefKind::Variant(variant) => {
                    self.lift_variant(
                        id,
                        &abi::abi(self.resolve, ty),
                        variant.cases.iter().map(|c| c.ty),
                        context,
                        value,
                    );
                }
                TypeDefKind::Enum(en) => {
                    self.lift_variant(
                        id,
                        &abi::abi(self.resolve, ty),
                        en.cases.iter().map(|_| None),
                        context,
                        value,
                    );
                }
                TypeDefKind::Union(un) => {
                    self.lift_variant(
                        id,
                        &abi::abi(self.resolve, ty),
                        un.cases.iter().map(|c| Some(c.ty)),
                        context,
                        value,
                    );
                }
                TypeDefKind::Option(some) => {
                    self.lift_variant(
                        self.get_option_type(*some),
                        &abi::abi(self.resolve, ty),
                        [None, Some(*some)],
                        context,
                        value,
                    );
                }
                TypeDefKind::Result(result) => {
                    self.lift_variant(
                        self.result_type.unwrap(),
                        &abi::abi(self.resolve, ty),
                        [result.ok, result.err],
                        context,
                        value,
                    );
                }
                TypeDefKind::Flags(flags) => {
                    self.lift_record_onto_stack(
                        id,
                        flags.types().collect::<Vec<_>>().into_iter(),
                        context,
                        value,
                    );
                }
                TypeDefKind::Tuple(tuple) => {
                    self.lift_record_onto_stack(
                        *self.tuple_types.get(&tuple.types.len()).unwrap(),
                        tuple.types.iter().copied(),
                        context,
                        value,
                    );
                }
                TypeDefKind::List(ty) => {
                    let source = value[0];
                    let length = value[1];

                    let abi = abi::abi(self.resolve, *ty);

                    if let Type::U8 | Type::S8 = ty {
                        self.push(Ins::LocalGet(context));
                        self.push(Ins::LocalGet(source));
                        self.push(Ins::LocalGet(length));
                        self.link_call(Link::MakeBytes);
                    } else {
                        let index = self.push_local(ValType::I32);
                        let element_source = self.push_local(ValType::I32);
                        let destination = self.push_local(ValType::I32);

                        self.push(Ins::LocalGet(context));
                        self.link_call(Link::MakeList);
                        self.push(Ins::LocalSet(destination));

                        self.push(Ins::I32Const(0));
                        self.push(Ins::LocalSet(index));

                        self.push(Ins::Loop(BlockType::Empty));

                        self.push(Ins::LocalGet(index));
                        self.push(Ins::LocalGet(length));
                        self.push(Ins::I32Ne);

                        self.push(Ins::If(BlockType::Empty));

                        self.push(Ins::LocalGet(source));
                        self.push(Ins::LocalGet(index));
                        self.push(Ins::I32Const(abi.size.try_into().unwrap()));
                        self.push(Ins::I32Mul);
                        self.push(Ins::I32Add);
                        self.push(Ins::LocalSet(element_source));

                        self.push(Ins::LocalGet(context));
                        self.push(Ins::LocalGet(destination));

                        self.load(*ty, context, element_source);

                        self.link_call(Link::ListAppend);

                        self.push(Ins::LocalGet(index));
                        self.push(Ins::I32Const(1));
                        self.push(Ins::I32Add);
                        self.push(Ins::LocalSet(index));

                        self.push(Ins::Br(1));

                        self.push(Ins::End);

                        self.push(Ins::End);

                        self.push(Ins::LocalGet(destination));

                        self.pop_local(destination, ValType::I32);
                        self.pop_local(element_source, ValType::I32);
                        self.pop_local(index, ValType::I32);
                    }
                }
                TypeDefKind::Type(ty) => self.lift(*ty, context, value),
                kind => todo!("{kind:?}"),
            },
        }
    }

    fn lift_record(
        &mut self,
        types: impl IntoIterator<Item = Type>,
        context: u32,
        source: &[u32],
        destination: u32,
    ) {
        let mut lift_index = 0;
        let mut store_offset = 0;
        for ty in types {
            let flat_count = abi::abi(self.resolve, ty).flattened.len();

            self.push(Ins::LocalGet(destination));
            self.lift(ty, context, &source[lift_index..][..flat_count]);
            self.push(Ins::I32Store(mem_arg(
                store_offset,
                WORD_ALIGN.try_into().unwrap(),
            )));

            lift_index += flat_count;
            store_offset += u64::try_from(WORD_SIZE).unwrap();
        }
    }

    fn lift_record_onto_stack(
        &mut self,
        id: TypeId,
        types: impl ExactSizeIterator<Item = Type>,
        context: u32,
        source: &[u32],
    ) {
        let len = types.len();
        self.push_stack(len * WORD_SIZE);
        let destination = self.push_local(ValType::I32);

        self.get_stack();
        self.push(Ins::LocalSet(destination));

        self.lift_record(types, context, source, destination);

        self.push(Ins::LocalGet(context));
        self.push(Ins::I32Const(
            self.types.get_index_of(&id).unwrap().try_into().unwrap(),
        ));
        self.get_stack();
        self.push(Ins::I32Const(len.try_into().unwrap()));
        self.link_call(Link::Init);

        self.pop_local(destination, ValType::I32);
        self.pop_stack(len * WORD_SIZE);
    }

    fn lift_variant(
        &mut self,
        id: TypeId,
        abi: &Abi,
        types: impl IntoIterator<Item = Option<Type>>,
        context: u32,
        source: &[u32],
    ) {
        self.push_stack(WORD_SIZE * 2);

        let types = types.into_iter().collect::<Vec<_>>();

        self.push(Ins::LocalGet(context));
        self.push(Ins::I32Const(
            self.types.get_index_of(&id).unwrap().try_into().unwrap(),
        ));
        self.get_stack();
        self.push(Ins::I32Const(2));

        self.get_stack();
        self.push(Ins::LocalGet(context));
        self.push(Ins::LocalGet(source[0]));
        self.link_call(Link::LiftI32);
        self.push(Ins::I32Store(mem_arg(0, WORD_ALIGN.try_into().unwrap())));

        self.get_stack();
        self.search_variant(
            BlockType::Result(ValType::I32),
            &types,
            source[0],
            |_, ty| ty.is_some(),
            |this, ty| {
                if let Some(ty) = ty {
                    let (source, locals) = this.convert_all(abi, ty, &source[1..]);

                    this.lift(ty, context, &source);

                    for (local, ty) in locals.into_iter().rev() {
                        this.pop_local(local, ty);
                    }
                } else {
                    this.push(Ins::LocalGet(context));
                    this.link_call(Link::None);
                }
            },
        );
        self.push(Ins::I32Store(mem_arg(
            WORD_SIZE.try_into().unwrap(),
            WORD_ALIGN.try_into().unwrap(),
        )));

        self.link_call(Link::Init);

        self.pop_stack(WORD_SIZE * 2);
    }

    fn load(&mut self, ty: Type, context: u32, source: u32) {
        match ty {
            Type::Bool | Type::U8 => {
                let value = self.push_local(ValType::I32);
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load8U(mem_arg(0, 0)));
                self.push(Ins::LocalSet(value));
                self.lift(ty, context, &[value]);
                self.pop_local(value, ValType::I32);
            }
            Type::S8 => {
                let value = self.push_local(ValType::I32);
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load8S(mem_arg(0, 0)));
                self.push(Ins::LocalSet(value));
                self.lift(ty, context, &[value]);
                self.pop_local(value, ValType::I32);
            }
            Type::U16 => {
                let value = self.push_local(ValType::I32);
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load16U(mem_arg(0, 1)));
                self.push(Ins::LocalSet(value));
                self.lift(ty, context, &[value]);
                self.pop_local(value, ValType::I32);
            }
            Type::S16 => {
                let value = self.push_local(ValType::I32);
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load16S(mem_arg(0, 1)));
                self.push(Ins::LocalSet(value));
                self.lift(ty, context, &[value]);
                self.pop_local(value, ValType::I32);
            }
            Type::U32 | Type::S32 | Type::Char => {
                let value = self.push_local(ValType::I32);
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load(mem_arg(0, 2)));
                self.push(Ins::LocalSet(value));
                self.lift(ty, context, &[value]);
                self.pop_local(value, ValType::I32);
            }
            Type::U64 | Type::S64 => {
                let value = self.push_local(ValType::I64);
                self.push(Ins::LocalGet(source));
                self.push(Ins::I64Load(mem_arg(0, 3)));
                self.push(Ins::LocalSet(value));
                self.lift(ty, context, &[value]);
                self.pop_local(value, ValType::I64);
            }
            Type::Float32 => {
                let value = self.push_local(ValType::F32);
                self.push(Ins::LocalGet(source));
                self.push(Ins::F32Load(mem_arg(0, 2)));
                self.push(Ins::LocalSet(value));
                self.lift(ty, context, &[value]);
                self.pop_local(value, ValType::F32);
            }
            Type::Float64 => {
                let value = self.push_local(ValType::F64);
                self.push(Ins::LocalGet(source));
                self.push(Ins::F64Load(mem_arg(0, 3)));
                self.push(Ins::LocalSet(value));
                self.lift(ty, context, &[value]);
                self.pop_local(value, ValType::F64);
            }
            Type::String => {
                self.push(Ins::LocalGet(context));
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load(mem_arg(0, WORD_ALIGN.try_into().unwrap())));
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load(mem_arg(
                    WORD_SIZE.try_into().unwrap(),
                    WORD_ALIGN.try_into().unwrap(),
                )));
                self.link_call(Link::LiftString);
            }
            Type::Id(id) => match &self.resolve.types[id].kind {
                TypeDefKind::Record(record) => {
                    self.load_record_onto_stack(
                        id,
                        record.fields.iter().map(|f| f.ty),
                        context,
                        source,
                    );
                }
                TypeDefKind::Variant(variant) => {
                    self.load_variant(
                        id,
                        &abi::abi(self.resolve, ty),
                        variant.cases.iter().map(|c| c.ty),
                        context,
                        source,
                    );
                }
                TypeDefKind::Enum(en) => {
                    self.load_variant(
                        id,
                        &abi::abi(self.resolve, ty),
                        en.cases.iter().map(|_| None),
                        context,
                        source,
                    );
                }
                TypeDefKind::Union(un) => {
                    self.load_variant(
                        id,
                        &abi::abi(self.resolve, ty),
                        un.cases.iter().map(|c| Some(c.ty)),
                        context,
                        source,
                    );
                }
                TypeDefKind::Option(some) => {
                    self.load_variant(
                        self.get_option_type(*some),
                        &abi::abi(self.resolve, ty),
                        [None, Some(*some)],
                        context,
                        source,
                    );
                }
                TypeDefKind::Result(result) => {
                    self.load_variant(
                        self.result_type.unwrap(),
                        &abi::abi(self.resolve, ty),
                        [result.ok, result.err],
                        context,
                        source,
                    );
                }
                TypeDefKind::Flags(flags) => {
                    self.load_record_onto_stack(
                        id,
                        flags.types().collect::<Vec<_>>().into_iter(),
                        context,
                        source,
                    );
                }
                TypeDefKind::Tuple(tuple) => {
                    self.load_record_onto_stack(
                        *self.tuple_types.get(&tuple.types.len()).unwrap(),
                        tuple.types.iter().copied(),
                        context,
                        source,
                    );
                }
                TypeDefKind::List(_) => {
                    let body = self.push_local(ValType::I32);
                    let length = self.push_local(ValType::I32);

                    self.push(Ins::LocalGet(source));
                    self.push(Ins::I32Load(mem_arg(0, WORD_ALIGN.try_into().unwrap())));
                    self.push(Ins::LocalSet(body));

                    self.push(Ins::LocalGet(source));
                    self.push(Ins::I32Load(mem_arg(
                        WORD_SIZE.try_into().unwrap(),
                        WORD_ALIGN.try_into().unwrap(),
                    )));
                    self.push(Ins::LocalSet(length));

                    self.lift(ty, context, &[body, length]);

                    self.pop_local(length, ValType::I32);
                    self.pop_local(body, ValType::I32);
                }
                TypeDefKind::Type(ty) => self.load(*ty, context, source),
                kind => todo!("{kind:?}"),
            },
        }
    }

    fn load_record(
        &mut self,
        types: impl IntoIterator<Item = Type>,
        context: u32,
        source: u32,
        destination: u32,
    ) {
        let mut load_offset = 0;
        let mut store_offset = 0;
        for ty in types {
            let field_source = self.push_local(ValType::I32);

            let abi = abi::abi(self.resolve, ty);
            load_offset = abi::align(load_offset, abi.align);

            self.push(Ins::LocalGet(source));
            self.push(Ins::I32Const(load_offset.try_into().unwrap()));
            self.push(Ins::I32Add);
            self.push(Ins::LocalSet(field_source));
            self.push(Ins::LocalGet(destination));
            self.load(ty, context, field_source);
            self.push(Ins::I32Store(mem_arg(
                store_offset,
                WORD_ALIGN.try_into().unwrap(),
            )));

            load_offset += abi.size;
            store_offset += u64::try_from(WORD_SIZE).unwrap();

            self.pop_local(field_source, ValType::I32);
        }
    }

    fn load_record_onto_stack(
        &mut self,
        id: TypeId,
        types: impl ExactSizeIterator<Item = Type>,
        context: u32,
        source: u32,
    ) {
        let len = types.len();
        self.push_stack(len * WORD_SIZE);
        let destination = self.push_local(ValType::I32);

        self.get_stack();
        self.push(Ins::LocalSet(destination));

        self.load_record(types, context, source, destination);

        self.push(Ins::LocalGet(context));
        self.push(Ins::I32Const(
            self.types.get_index_of(&id).unwrap().try_into().unwrap(),
        ));
        self.get_stack();
        self.push(Ins::I32Const(len.try_into().unwrap()));
        self.link_call(Link::Init);

        self.pop_local(destination, ValType::I32);
        self.pop_stack(len * WORD_SIZE);
    }

    fn load_variant(
        &mut self,
        id: TypeId,
        abi: &Abi,
        types: impl IntoIterator<Item = Option<Type>>,
        context: u32,
        source: u32,
    ) {
        self.push_stack(WORD_SIZE * 2);

        let types = types.into_iter().collect::<Vec<_>>();
        let discriminant_size = abi::discriminant_size(types.len());
        let discriminant = self.push_local(ValType::I32);

        self.push(Ins::LocalGet(context));
        self.push(Ins::I32Const(
            self.types.get_index_of(&id).unwrap().try_into().unwrap(),
        ));
        self.get_stack();
        self.push(Ins::I32Const(2));

        self.get_stack();
        self.push(Ins::LocalGet(context));
        self.push(Ins::LocalGet(source));
        match discriminant_size {
            1 => self.push(Ins::I32Load8U(mem_arg(0, 0))),
            2 => self.push(Ins::I32Load16U(mem_arg(0, 1))),
            4 => self.push(Ins::I32Load(mem_arg(0, 2))),
            _ => unreachable!(),
        }
        self.push(Ins::LocalTee(discriminant));
        self.link_call(Link::LiftI32);
        self.push(Ins::I32Store(mem_arg(0, WORD_ALIGN.try_into().unwrap())));

        self.get_stack();
        if types.iter().any(Option::is_some) {
            let payload_source = self.push_local(ValType::I32);

            self.push(Ins::LocalGet(source));
            self.push(Ins::I32Const(
                abi::align(discriminant_size, abi.align).try_into().unwrap(),
            ));
            self.push(Ins::I32Add);
            self.push(Ins::LocalSet(payload_source));

            self.search_variant(
                BlockType::Result(ValType::I32),
                &types,
                discriminant,
                |_, ty| ty.is_some(),
                |this, ty| {
                    if let Some(ty) = ty {
                        this.load(ty, context, payload_source);
                    } else {
                        this.push(Ins::LocalGet(context));
                        this.link_call(Link::None);
                    }
                },
            );

            self.pop_local(payload_source, ValType::I32);
        } else {
            self.push(Ins::LocalGet(context));
            self.link_call(Link::None);
        }
        self.push(Ins::I32Store(mem_arg(
            WORD_SIZE.try_into().unwrap(),
            WORD_ALIGN.try_into().unwrap(),
        )));

        self.link_call(Link::Init);

        self.pop_stack(WORD_SIZE * 2);
        self.pop_local(discriminant, ValType::I32);
    }

    fn load_copy(&mut self, ty: Type, source: u32) {
        match ty {
            Type::Bool | Type::U8 => {
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load8U(mem_arg(0, 0)));
            }
            Type::S8 => {
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load8S(mem_arg(0, 0)));
            }
            Type::U16 => {
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load16U(mem_arg(0, 1)));
            }
            Type::S16 => {
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load16S(mem_arg(0, 1)));
            }
            Type::U32 | Type::S32 | Type::Char => {
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load(mem_arg(0, 2)));
            }
            Type::U64 | Type::S64 => {
                self.push(Ins::LocalGet(source));
                self.push(Ins::I64Load(mem_arg(0, 3)));
            }
            Type::Float32 => {
                self.push(Ins::LocalGet(source));
                self.push(Ins::F32Load(mem_arg(0, 2)));
            }
            Type::Float64 => {
                self.push(Ins::LocalGet(source));
                self.push(Ins::F64Load(mem_arg(0, 3)));
            }
            Type::String => {
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load(mem_arg(0, WORD_ALIGN.try_into().unwrap())));
                self.push(Ins::LocalGet(source));
                self.push(Ins::I32Load(mem_arg(
                    WORD_SIZE.try_into().unwrap(),
                    WORD_ALIGN.try_into().unwrap(),
                )));
            }
            Type::Id(id) => match &self.resolve.types[id].kind {
                TypeDefKind::Record(record) => {
                    self.load_copy_record(record.fields.iter().map(|f| f.ty), source);
                }
                TypeDefKind::Variant(variant) => {
                    self.load_copy_variant(
                        &abi::abi(self.resolve, ty),
                        variant.cases.iter().map(|c| c.ty),
                        source,
                    );
                }
                TypeDefKind::Enum(en) => {
                    self.load_copy_variant(
                        &abi::abi(self.resolve, ty),
                        en.cases.iter().map(|_| None),
                        source,
                    );
                }
                TypeDefKind::Union(un) => {
                    self.load_copy_variant(
                        &abi::abi(self.resolve, ty),
                        un.cases.iter().map(|c| Some(c.ty)),
                        source,
                    );
                }
                TypeDefKind::Option(some) => {
                    self.load_copy_variant(
                        &abi::abi(self.resolve, ty),
                        [None, Some(*some)],
                        source,
                    );
                }
                TypeDefKind::Result(result) => {
                    self.load_copy_variant(
                        &abi::abi(self.resolve, ty),
                        [result.ok, result.err],
                        source,
                    );
                }
                TypeDefKind::Flags(flags) => {
                    self.load_copy_record(flags.types(), source);
                }
                TypeDefKind::Tuple(tuple) => {
                    self.load_copy_record(tuple.types.iter().copied(), source);
                }
                TypeDefKind::List(_) => {
                    self.push(Ins::LocalGet(source));
                    self.push(Ins::I32Load(mem_arg(0, WORD_ALIGN.try_into().unwrap())));
                    self.push(Ins::LocalGet(source));
                    self.push(Ins::I32Load(mem_arg(
                        WORD_SIZE.try_into().unwrap(),
                        WORD_ALIGN.try_into().unwrap(),
                    )));
                }
                TypeDefKind::Type(ty) => self.load_copy(*ty, source),
                kind => todo!("{kind:?}"),
            },
        }
    }

    fn load_copy_record(&mut self, types: impl IntoIterator<Item = Type>, source: u32) {
        let mut load_offset = 0;
        for ty in types {
            let field_source = self.push_local(ValType::I32);

            let abi = abi::abi(self.resolve, ty);
            load_offset = abi::align(load_offset, abi.align);

            self.push(Ins::LocalGet(source));
            self.push(Ins::I32Const(load_offset.try_into().unwrap()));
            self.push(Ins::I32Add);
            self.push(Ins::LocalSet(field_source));

            self.load_copy(ty, field_source);

            load_offset += abi.size;

            self.pop_local(field_source, ValType::I32);
        }
    }

    fn zero(&mut self, ty: ValType) {
        self.push(match ty {
            ValType::I32 => Ins::I32Const(0),
            ValType::I64 => Ins::I64Const(0),
            ValType::F32 => Ins::F32Const(0.0),
            ValType::F64 => Ins::F64Const(0.0),
            _ => unreachable!(),
        })
    }

    fn load_copy_variant(
        &mut self,
        abi: &Abi,
        types: impl IntoIterator<Item = Option<Type>>,
        source: u32,
    ) {
        let types = types.into_iter().collect::<Vec<_>>();
        let discriminant_size = abi::discriminant_size(types.len());

        self.push(Ins::LocalGet(source));
        match discriminant_size {
            1 => self.push(Ins::I32Load8U(mem_arg(0, 0))),
            2 => self.push(Ins::I32Load16U(mem_arg(0, 1))),
            4 => self.push(Ins::I32Load(mem_arg(0, 2))),
            _ => unreachable!(),
        }

        if types.iter().any(Option::is_some) {
            let discriminant = self.push_local(ValType::I32);
            let payload_source = self.push_local(ValType::I32);
            let destination = abi
                .flattened
                .iter()
                .skip(1)
                .map(|&ty| self.push_local(ty))
                .collect::<Vec<_>>();

            self.push(Ins::LocalTee(discriminant));

            self.push(Ins::LocalGet(source));
            self.push(Ins::I32Const(
                abi::align(discriminant_size, abi.align).try_into().unwrap(),
            ));
            self.push(Ins::I32Add);
            self.push(Ins::LocalSet(payload_source));

            self.search_variant(
                BlockType::Empty,
                &types,
                discriminant,
                |_, ty| ty.is_some(),
                |this, ty| {
                    if let Some(ty) = ty {
                        this.load_copy(ty, payload_source);

                        let payload_abi = abi::abi(this.resolve, ty);
                        for ((payload_type, joined_type), local) in payload_abi
                            .flattened
                            .iter()
                            .zip(abi.flattened.iter().skip(1))
                            .zip(&destination)
                            .rev()
                        {
                            if payload_type != joined_type {
                                this.convert(*payload_type, *joined_type);
                            }
                            this.push(Ins::LocalSet(*local));
                        }

                        for (joined_type, local) in abi
                            .flattened
                            .iter()
                            .skip(1)
                            .zip(&destination)
                            .skip(payload_abi.flattened.len())
                        {
                            this.zero(*joined_type);
                            this.push(Ins::LocalSet(*local));
                        }
                    }
                },
            );

            for &local in &destination {
                self.push(Ins::LocalGet(local));
            }

            for (local, ty) in destination
                .into_iter()
                .zip(abi.flattened.iter().skip(1))
                .rev()
            {
                self.pop_local(local, *ty);
            }
            self.pop_local(payload_source, ValType::I32);
            self.pop_local(discriminant, ValType::I32);
        }
    }

    fn free_lowered(&mut self, ty: Type, value: &[u32]) {
        match ty {
            Type::Bool
            | Type::U8
            | Type::U16
            | Type::U32
            | Type::S8
            | Type::S16
            | Type::S32
            | Type::Char
            | Type::U64
            | Type::S64
            | Type::Float32
            | Type::Float64 => {}

            Type::String => {
                self.push(Ins::LocalGet(value[0]));
                self.push(Ins::LocalGet(value[1]));
                self.push(Ins::I32Const(1));
                self.link_call(Link::Free);
            }

            Type::Id(id) => match &self.resolve.types[id].kind {
                TypeDefKind::Record(record) => {
                    self.free_lowered_record(record.fields.iter().map(|f| f.ty), value);
                }
                TypeDefKind::Variant(variant) => {
                    self.free_lowered_variant(
                        &abi::abi(self.resolve, ty),
                        variant.cases.iter().map(|c| c.ty),
                        value,
                    );
                }
                TypeDefKind::Enum(en) => {
                    self.free_lowered_variant(
                        &abi::abi(self.resolve, ty),
                        en.cases.iter().map(|_| None),
                        value,
                    );
                }
                TypeDefKind::Union(un) => {
                    self.free_lowered_variant(
                        &abi::abi(self.resolve, ty),
                        un.cases.iter().map(|c| Some(c.ty)),
                        value,
                    );
                }
                TypeDefKind::Option(some) => {
                    self.free_lowered_variant(
                        &abi::abi(self.resolve, ty),
                        [None, Some(*some)],
                        value,
                    );
                }
                TypeDefKind::Result(result) => {
                    self.free_lowered_variant(
                        &abi::abi(self.resolve, ty),
                        [result.ok, result.err],
                        value,
                    );
                }
                TypeDefKind::Flags(flags) => {
                    self.free_lowered_record(flags.types(), value);
                }
                TypeDefKind::Tuple(tuple) => {
                    self.free_lowered_record(tuple.types.iter().copied(), value);
                }
                TypeDefKind::List(ty) => {
                    let pointer = value[0];
                    let length = value[1];

                    let abi = abi::abi(self.resolve, *ty);

                    if abi::has_pointer(self.resolve, *ty) {
                        let index = self.push_local(ValType::I32);
                        let element_pointer = self.push_local(ValType::I32);

                        self.push(Ins::I32Const(0));
                        self.push(Ins::LocalSet(index));

                        self.push(Ins::Loop(BlockType::Empty));

                        self.push(Ins::LocalGet(index));
                        self.push(Ins::LocalGet(length));
                        self.push(Ins::I32Ne);

                        self.push(Ins::If(BlockType::Empty));

                        self.push(Ins::LocalGet(pointer));
                        self.push(Ins::LocalGet(index));
                        self.push(Ins::I32Const(abi.size.try_into().unwrap()));
                        self.push(Ins::I32Mul);
                        self.push(Ins::I32Add);
                        self.push(Ins::LocalSet(element_pointer));

                        self.free_stored(*ty, element_pointer);

                        self.push(Ins::LocalGet(index));
                        self.push(Ins::I32Const(1));
                        self.push(Ins::I32Add);
                        self.push(Ins::LocalSet(index));

                        self.push(Ins::Br(1));

                        self.push(Ins::End);

                        self.push(Ins::End);

                        self.pop_local(element_pointer, ValType::I32);
                        self.pop_local(index, ValType::I32);
                    }

                    self.push(Ins::LocalGet(pointer));
                    self.push(Ins::LocalGet(length));
                    self.push(Ins::I32Const(abi.size.try_into().unwrap()));
                    self.push(Ins::I32Mul);
                    self.push(Ins::I32Const(abi.align.try_into().unwrap()));
                    self.link_call(Link::Free);
                }
                TypeDefKind::Type(ty) => self.free_lowered(*ty, value),
                kind => todo!("{kind:?}"),
            },
        }
    }

    fn free_lowered_record(&mut self, types: impl IntoIterator<Item = Type>, value: &[u32]) {
        let mut lift_index = 0;
        for ty in types {
            let flat_count = abi::abi(self.resolve, ty).flattened.len();

            self.free_lowered(ty, &value[lift_index..][..flat_count]);

            lift_index += flat_count;
        }
    }

    fn free_lowered_variant(
        &mut self,
        abi: &Abi,
        types: impl IntoIterator<Item = Option<Type>>,
        value: &[u32],
    ) {
        self.search_variant(
            BlockType::Empty,
            &types.into_iter().collect::<Vec<_>>(),
            value[0],
            |this, ty| {
                ty.map(|ty| abi::has_pointer(this.resolve, ty))
                    .unwrap_or(false)
            },
            |this, ty| {
                if let Some(ty) = ty {
                    let (value, locals) = this.convert_all(abi, ty, &value[1..]);

                    this.free_lowered(ty, &value);

                    for (local, ty) in locals.into_iter().rev() {
                        this.pop_local(local, ty);
                    }
                }
            },
        )
    }

    fn free_stored(&mut self, ty: Type, value: u32) {
        match ty {
            Type::Bool
            | Type::U8
            | Type::U16
            | Type::U32
            | Type::S8
            | Type::S16
            | Type::S32
            | Type::Char
            | Type::U64
            | Type::S64
            | Type::Float32
            | Type::Float64 => {}

            Type::String => {
                self.push(Ins::LocalGet(value));
                self.push(Ins::I32Load(mem_arg(0, WORD_ALIGN.try_into().unwrap())));
                self.push(Ins::LocalGet(value));
                self.push(Ins::I32Load(mem_arg(
                    WORD_SIZE.try_into().unwrap(),
                    WORD_ALIGN.try_into().unwrap(),
                )));
                self.push(Ins::I32Const(1));
                self.link_call(Link::Free);
            }

            Type::Id(id) => match &self.resolve.types[id].kind {
                TypeDefKind::Record(record) => {
                    self.free_stored_record(record.fields.iter().map(|f| f.ty), value);
                }
                TypeDefKind::Variant(variant) => {
                    self.free_stored_variant(
                        &abi::abi(self.resolve, ty),
                        variant.cases.iter().map(|c| c.ty),
                        value,
                    );
                }
                TypeDefKind::Enum(en) => {
                    self.free_stored_variant(
                        &abi::abi(self.resolve, ty),
                        en.cases.iter().map(|_| None),
                        value,
                    );
                }
                TypeDefKind::Union(un) => {
                    self.free_stored_variant(
                        &abi::abi(self.resolve, ty),
                        un.cases.iter().map(|c| Some(c.ty)),
                        value,
                    );
                }
                TypeDefKind::Option(some) => {
                    self.free_stored_variant(
                        &abi::abi(self.resolve, ty),
                        [None, Some(*some)],
                        value,
                    );
                }
                TypeDefKind::Result(result) => {
                    self.free_stored_variant(
                        &abi::abi(self.resolve, ty),
                        [result.ok, result.err],
                        value,
                    );
                }
                TypeDefKind::Flags(flags) => {
                    self.free_stored_record(flags.types(), value);
                }
                TypeDefKind::Tuple(tuple) => {
                    self.free_stored_record(tuple.types.iter().copied(), value);
                }
                TypeDefKind::List(ty) => {
                    let abi = abi::abi(self.resolve, *ty);

                    let body = self.push_local(ValType::I32);
                    let length = self.push_local(ValType::I32);

                    self.push(Ins::LocalGet(value));
                    self.push(Ins::I32Load(mem_arg(0, WORD_ALIGN.try_into().unwrap())));
                    self.push(Ins::LocalSet(body));

                    self.push(Ins::LocalGet(value));
                    self.push(Ins::I32Load(mem_arg(
                        WORD_SIZE.try_into().unwrap(),
                        WORD_ALIGN.try_into().unwrap(),
                    )));
                    self.push(Ins::LocalSet(length));

                    if abi::has_pointer(self.resolve, *ty) {
                        let index = self.push_local(ValType::I32);
                        let element_value = self.push_local(ValType::I32);

                        self.push(Ins::I32Const(0));
                        self.push(Ins::LocalSet(index));

                        self.push(Ins::Loop(BlockType::Empty));

                        self.push(Ins::LocalGet(index));
                        self.push(Ins::LocalGet(length));
                        self.push(Ins::I32Ne);

                        self.push(Ins::If(BlockType::Empty));

                        self.push(Ins::LocalGet(body));
                        self.push(Ins::LocalGet(index));
                        self.push(Ins::I32Const(abi.size.try_into().unwrap()));
                        self.push(Ins::I32Mul);
                        self.push(Ins::I32Add);
                        self.push(Ins::LocalSet(element_value));

                        self.free_stored(*ty, element_value);

                        self.push(Ins::LocalGet(index));
                        self.push(Ins::I32Const(1));
                        self.push(Ins::I32Add);
                        self.push(Ins::LocalSet(index));

                        self.push(Ins::Br(1));

                        self.push(Ins::End);

                        self.push(Ins::End);

                        self.pop_local(element_value, ValType::I32);
                        self.pop_local(index, ValType::I32);
                    }

                    self.push(Ins::LocalGet(body));
                    self.push(Ins::LocalGet(length));
                    self.push(Ins::I32Const(abi.size.try_into().unwrap()));
                    self.push(Ins::I32Mul);
                    self.push(Ins::I32Const(abi.align.try_into().unwrap()));
                    self.link_call(Link::Free);

                    self.pop_local(length, ValType::I32);
                    self.pop_local(body, ValType::I32);
                }
                TypeDefKind::Type(ty) => self.free_stored(*ty, value),
                kind => todo!("{kind:?}"),
            },
        }
    }

    fn free_stored_record(&mut self, types: impl IntoIterator<Item = Type>, value: u32) {
        let types = types.into_iter().collect::<Vec<_>>();

        let mut load_offset = 0;
        for ty in types {
            let abi = abi::abi(self.resolve, ty);
            load_offset = abi::align(load_offset, abi.align);

            if abi::has_pointer(self.resolve, ty) {
                let field_value = self.push_local(ValType::I32);

                self.push(Ins::LocalGet(value));
                self.push(Ins::I32Const(load_offset.try_into().unwrap()));
                self.push(Ins::I32Add);
                self.push(Ins::LocalSet(field_value));

                self.free_stored(ty, field_value);

                self.pop_local(field_value, ValType::I32);
            }

            load_offset += abi.size;
        }
    }

    fn free_stored_variant(
        &mut self,
        abi: &Abi,
        types: impl IntoIterator<Item = Option<Type>>,
        value: u32,
    ) {
        let types = types.into_iter().collect::<Vec<_>>();
        let discriminant_size = abi::discriminant_size(types.len());
        let predicate = |this: &Self, ty: Option<Type>| {
            ty.map(|ty| abi::has_pointer(this.resolve, ty))
                .unwrap_or(false)
        };

        if types.iter().any(|ty| predicate(self, *ty)) {
            let discriminant = self.push_local(ValType::I32);

            self.push(Ins::LocalGet(value));
            match discriminant_size {
                1 => self.push(Ins::I32Load8U(mem_arg(0, 0))),
                2 => self.push(Ins::I32Load16U(mem_arg(0, 1))),
                4 => self.push(Ins::I32Load(mem_arg(0, 2))),
                _ => unreachable!(),
            }
            self.push(Ins::LocalSet(discriminant));

            let payload_value = self.push_local(ValType::I32);

            self.push(Ins::LocalGet(value));
            self.push(Ins::I32Const(
                abi::align(discriminant_size, abi.align).try_into().unwrap(),
            ));
            self.push(Ins::I32Add);
            self.push(Ins::LocalSet(payload_value));

            self.search_variant(
                BlockType::Empty,
                &types,
                discriminant,
                predicate,
                |this, ty| {
                    if let Some(ty) = ty {
                        this.free_stored(ty, payload_value);
                    }
                },
            );

            self.pop_local(payload_value, ValType::I32);
            self.pop_local(discriminant, ValType::I32);
        }
    }

    fn get_option_type(&self, some: Type) -> TypeId {
        if abi::is_option(self.resolve, some) {
            self.nesting_option_type.unwrap()
        } else {
            self.option_type.unwrap()
        }
    }
}

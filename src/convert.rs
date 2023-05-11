use {
    anyhow::{Error, Result},
    wasm_encoder::{
        BlockType, ConstExpr, Elements, Encode as _, EntityType, ExportKind, GlobalType, HeapType,
        MemArg, MemoryType, RefType, TableType, TagKind, TagType, ValType,
    },
    wasmparser::{BinaryReader, ExternalKind, TypeRef, VisitOperator},
};

pub struct IntoGlobalType(pub wasmparser::GlobalType);

impl From<IntoGlobalType> for GlobalType {
    fn from(val: IntoGlobalType) -> Self {
        GlobalType {
            val_type: IntoValType(val.0.content_type).into(),
            mutable: val.0.mutable,
        }
    }
}

pub struct IntoBlockType(pub wasmparser::BlockType);

impl From<IntoBlockType> for BlockType {
    fn from(val: IntoBlockType) -> Self {
        match val.0 {
            wasmparser::BlockType::Empty => BlockType::Empty,
            wasmparser::BlockType::Type(ty) => BlockType::Result(IntoValType(ty).into()),
            wasmparser::BlockType::FuncType(ty) => BlockType::FunctionType(ty),
        }
    }
}

pub struct IntoMemArg(pub wasmparser::MemArg);

impl From<IntoMemArg> for MemArg {
    fn from(val: IntoMemArg) -> Self {
        MemArg {
            offset: val.0.offset,
            align: val.0.align.into(),
            memory_index: val.0.memory,
        }
    }
}

pub struct IntoTableType(pub wasmparser::TableType);

impl From<IntoTableType> for TableType {
    fn from(val: IntoTableType) -> Self {
        TableType {
            element_type: IntoRefType(val.0.element_type).into(),
            minimum: val.0.initial,
            maximum: val.0.maximum,
        }
    }
}

pub struct IntoHeapType(pub wasmparser::HeapType);

impl From<IntoHeapType> for HeapType {
    fn from(val: IntoHeapType) -> Self {
        match val.0 {
            wasmparser::HeapType::Func => HeapType::Func,
            wasmparser::HeapType::Extern => HeapType::Extern,
            wasmparser::HeapType::TypedFunc(index) => HeapType::TypedFunc(index.into()),
        }
    }
}

pub struct IntoRefType(pub wasmparser::RefType);

impl From<IntoRefType> for RefType {
    fn from(val: IntoRefType) -> Self {
        RefType {
            nullable: val.0.nullable,
            heap_type: IntoHeapType(val.0.heap_type).into(),
        }
    }
}

pub struct IntoValType(pub wasmparser::ValType);

impl From<IntoValType> for ValType {
    fn from(val: IntoValType) -> Self {
        match val.0 {
            wasmparser::ValType::I32 => ValType::I32,
            wasmparser::ValType::I64 => ValType::I64,
            wasmparser::ValType::F32 => ValType::F32,
            wasmparser::ValType::F64 => ValType::F64,
            wasmparser::ValType::V128 => ValType::V128,
            wasmparser::ValType::Ref(ty) => ValType::Ref(IntoRefType(ty).into()),
        }
    }
}

pub struct IntoTagKind(pub wasmparser::TagKind);

impl From<IntoTagKind> for TagKind {
    fn from(val: IntoTagKind) -> Self {
        match val.0 {
            wasmparser::TagKind::Exception => TagKind::Exception,
        }
    }
}

pub struct IntoEntityType(pub TypeRef);

impl From<IntoEntityType> for EntityType {
    fn from(val: IntoEntityType) -> Self {
        match val.0 {
            TypeRef::Func(index) => EntityType::Function(index),
            TypeRef::Table(ty) => EntityType::Table(TableType {
                element_type: IntoRefType(ty.element_type).into(),
                minimum: ty.initial,
                maximum: ty.maximum,
            }),
            TypeRef::Memory(ty) => EntityType::Memory(MemoryType {
                minimum: ty.initial,
                maximum: ty.maximum,
                memory64: ty.memory64,
                shared: ty.shared,
            }),
            TypeRef::Global(ty) => EntityType::Global(GlobalType {
                val_type: IntoValType(ty.content_type).into(),
                mutable: ty.mutable,
            }),
            TypeRef::Tag(ty) => EntityType::Tag(TagType {
                kind: IntoTagKind(ty.kind).into(),
                func_type_idx: ty.func_type_idx,
            }),
        }
    }
}

pub struct IntoExportKind(pub ExternalKind);

impl From<IntoExportKind> for ExportKind {
    fn from(val: IntoExportKind) -> Self {
        match val.0 {
            ExternalKind::Func => ExportKind::Func,
            ExternalKind::Table => ExportKind::Table,
            ExternalKind::Memory => ExportKind::Memory,
            ExternalKind::Global => ExportKind::Global,
            ExternalKind::Tag => ExportKind::Tag,
        }
    }
}

struct Visitor<F> {
    remap: F,
    buffer: Vec<u8>,
}

// Adapted from https://github.com/bytecodealliance/wasm-tools/blob/1e0052974277b3cce6c3703386e4e90291da2b24/crates/wit-component/src/gc.rs#L1118
macro_rules! define_encode {
    ($(@$p:ident $op:ident $({ $($arg:ident: $argty:ty),* })? => $visit:ident)*) => {
        $(
            #[allow(clippy::drop_copy)]
            fn $visit(&mut self $(, $($arg: $argty),*)?)  {
                #[allow(unused_imports)]
                use wasm_encoder::Instruction::*;
                $(
                    $(
                        let $arg = define_encode!(map self $arg $arg);
                    )*
                )?
                let insn = define_encode!(mk $op $($($arg)*)?);
                insn.encode(&mut self.buffer);
            }
        )*
    };

    // No-payload instructions are named the same in wasmparser as they are in
    // wasm-encoder
    (mk $op:ident) => ($op);

    // Instructions which need "special care" to map from wasmparser to
    // wasm-encoder
    (mk BrTable $arg:ident) => ({
        BrTable($arg.0, $arg.1)
    });
    (mk CallIndirect $ty:ident $table:ident $table_byte:ident) => ({
        drop($table_byte);
        CallIndirect { ty: $ty, table: $table }
    });
    (mk ReturnCallIndirect $ty:ident $table:ident) => (
        ReturnCallIndirect { ty: $ty, table: $table }
    );
    (mk MemorySize $mem:ident $mem_byte:ident) => ({
        drop($mem_byte);
        MemorySize($mem)
    });
    (mk MemoryGrow $mem:ident $mem_byte:ident) => ({
        drop($mem_byte);
        MemoryGrow($mem)
    });
    (mk I32Const $v:ident) => (I32Const($v));
    (mk I64Const $v:ident) => (I64Const($v));
    (mk F32Const $v:ident) => (F32Const(f32::from_bits($v.bits())));
    (mk F64Const $v:ident) => (F64Const(f64::from_bits($v.bits())));
    (mk V128Const $v:ident) => (V128Const($v.i128()));

    // Catch-all for the translation of one payload argument which is typically
    // represented as a tuple-enum in wasm-encoder.
    (mk $op:ident $arg:ident) => ($op($arg));

    // Catch-all of everything else where the wasmparser fields are simply
    // translated to wasm-encoder fields.
    (mk $op:ident $($arg:ident)*) => ($op { $($arg),* });

    // Individual cases of mapping one argument type to another
    (map $self:ident $arg:ident memarg) => {IntoMemArg($arg).into()};
    (map $self:ident $arg:ident blockty) => {IntoBlockType($arg).into()};
    (map $self:ident $arg:ident hty) => {IntoHeapType($arg).into()};
    (map $self:ident $arg:ident tag_index) => {$arg};
    (map $self:ident $arg:ident relative_depth) => {$arg};
    (map $self:ident $arg:ident function_index) => {($self.remap)($arg)};
    (map $self:ident $arg:ident global_index) => {$arg};
    (map $self:ident $arg:ident mem) => {$arg};
    (map $self:ident $arg:ident src_mem) => {$arg};
    (map $self:ident $arg:ident dst_mem) => {$arg};
    (map $self:ident $arg:ident table) => {$arg};
    (map $self:ident $arg:ident table_index) => {$arg};
    (map $self:ident $arg:ident src_table) => {$arg};
    (map $self:ident $arg:ident dst_table) => {$arg};
    (map $self:ident $arg:ident type_index) => {$arg};
    (map $self:ident $arg:ident ty) => {IntoValType($arg).into()};
    (map $self:ident $arg:ident local_index) => {$arg};
    (map $self:ident $arg:ident lane) => {$arg};
    (map $self:ident $arg:ident lanes) => {$arg};
    (map $self:ident $arg:ident elem_index) => {$arg};
    (map $self:ident $arg:ident data_index) => {$arg};
    (map $self:ident $arg:ident table_byte) => {$arg};
    (map $self:ident $arg:ident mem_byte) => {$arg};
    (map $self:ident $arg:ident value) => {$arg};
    (map $self:ident $arg:ident targets) => ((
        $arg.targets().map(|i| i.unwrap()).collect::<Vec<_>>().into(),
        $arg.default(),
    ));
}

impl<'a, F: Fn(u32) -> u32> VisitOperator<'a> for Visitor<F> {
    type Output = ();

    wasmparser::for_each_operator!(define_encode);
}

pub fn visit(mut reader: BinaryReader<'_>, remap: impl Fn(u32) -> u32) -> Result<Vec<u8>> {
    let mut visitor = Visitor {
        remap,
        buffer: Vec::new(),
    };
    while !reader.eof() {
        reader.visit_operator(&mut visitor)?;
    }
    Ok(visitor.buffer)
}

pub fn const_expr(reader: BinaryReader<'_>, remap: impl Fn(u32) -> u32) -> Result<ConstExpr> {
    let mut bytes = visit(reader, remap)?;
    assert_eq!(bytes.pop(), Some(0xb));
    Ok(ConstExpr::raw(bytes))
}

pub enum MyElements {
    Functions(Vec<u32>),
    Expressions(Vec<ConstExpr>),
}

impl MyElements {
    pub fn as_elements(&self) -> Elements {
        match self {
            Self::Functions(v) => Elements::Functions(v),
            Self::Expressions(v) => Elements::Expressions(v),
        }
    }
}

impl<F: (Fn(u32) -> u32) + Copy> TryFrom<(wasmparser::ElementItems<'_>, F)> for MyElements {
    type Error = Error;

    fn try_from((val, remap): (wasmparser::ElementItems, F)) -> Result<MyElements> {
        Ok(match val {
            wasmparser::ElementItems::Functions(reader) => MyElements::Functions(
                reader
                    .into_iter()
                    .map(|f| f.map(remap))
                    .collect::<Result<_, _>>()?,
            ),
            wasmparser::ElementItems::Expressions(reader) => MyElements::Expressions(
                reader
                    .into_iter()
                    .map(|e| const_expr(e?.get_binary_reader(), remap))
                    .collect::<Result<_, _>>()?,
            ),
        })
    }
}

use {
    wasm_encoder::{
        BlockType, EntityType, ExportKind, GlobalType, HeapType, MemArg, MemoryType, RefType,
        TableType, TagKind, TagType, ValType,
    },
    wasmparser::{ExternalKind, TypeRef},
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

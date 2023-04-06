use {
    wasm_encoder::{
        BlockType, EntityType, ExportKind, GlobalType, HeapType, MemArg, MemoryType, RefType,
        TableType, TagKind, TagType, ValType,
    },
    wasmparser::{ExternalKind, TypeRef},
};

pub struct IntoGlobalType(pub wasmparser::GlobalType);

impl Into<GlobalType> for IntoGlobalType {
    fn into(self) -> GlobalType {
        GlobalType {
            val_type: IntoValType(self.0.content_type).into(),
            mutable: self.0.mutable,
        }
    }
}

pub struct IntoBlockType(pub wasmparser::BlockType);

impl Into<BlockType> for IntoBlockType {
    fn into(self) -> BlockType {
        match self.0 {
            wasmparser::BlockType::Empty => BlockType::Empty,
            wasmparser::BlockType::Type(ty) => BlockType::Result(IntoValType(ty).into()),
            wasmparser::BlockType::FuncType(ty) => BlockType::FunctionType(ty),
        }
    }
}

pub struct IntoMemArg(pub wasmparser::MemArg);

impl Into<MemArg> for IntoMemArg {
    fn into(self) -> MemArg {
        MemArg {
            offset: self.0.offset,
            align: self.0.align.into(),
            memory_index: self.0.memory,
        }
    }
}

pub struct IntoTableType(pub wasmparser::TableType);

impl Into<TableType> for IntoTableType {
    fn into(self) -> TableType {
        TableType {
            element_type: IntoRefType(self.0.element_type).into(),
            minimum: self.0.initial,
            maximum: self.0.maximum,
        }
    }
}

pub struct IntoHeapType(pub wasmparser::HeapType);

impl Into<HeapType> for IntoHeapType {
    fn into(self) -> HeapType {
        match self.0 {
            wasmparser::HeapType::Func => HeapType::Func,
            wasmparser::HeapType::Extern => HeapType::Extern,
            wasmparser::HeapType::TypedFunc(index) => HeapType::TypedFunc(index.into()),
        }
    }
}

pub struct IntoRefType(pub wasmparser::RefType);

impl Into<RefType> for IntoRefType {
    fn into(self) -> RefType {
        RefType {
            nullable: self.0.nullable,
            heap_type: IntoHeapType(self.0.heap_type).into(),
        }
    }
}

pub struct IntoValType(pub wasmparser::ValType);

impl Into<ValType> for IntoValType {
    fn into(self) -> ValType {
        match self.0 {
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

impl Into<TagKind> for IntoTagKind {
    fn into(self) -> TagKind {
        match self.0 {
            wasmparser::TagKind::Exception => TagKind::Exception,
        }
    }
}

pub struct IntoEntityType(pub TypeRef);

impl Into<EntityType> for IntoEntityType {
    fn into(self) -> EntityType {
        match self.0 {
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

impl Into<ExportKind> for IntoExportKind {
    fn into(self) -> ExportKind {
        match self.0 {
            ExternalKind::Func => ExportKind::Func,
            ExternalKind::Table => ExportKind::Table,
            ExternalKind::Memory => ExportKind::Memory,
            ExternalKind::Global => ExportKind::Global,
            ExternalKind::Tag => ExportKind::Tag,
        }
    }
}

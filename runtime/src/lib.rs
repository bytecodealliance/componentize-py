#![deny(warnings)]
#![expect(
    clippy::useless_conversion,
    reason = "some pyo3 macros produce code that does this"
)]
#![expect(
    clippy::not_unsafe_ptr_arg_deref,
    reason = "wit_dylib_ffi::export produces code that does this"
)]

use {
    anyhow::{Error, Result},
    bindings::{
        exports::exports::{
            self as exp, Constructor, FunctionExportKind, Guest, OptionKind, ResultRecord,
            ReturnStyle, Static, Symbols,
        },
        wasi::cli0_2_0::environment,
    },
    num_bigint::BigUint,
    once_cell::sync::OnceCell,
    pyo3::{
        Bound, IntoPyObject, Py, PyAny, PyErr, PyResult, Python,
        exceptions::PyAssertionError,
        intern,
        types::{
            PyAnyMethods, PyBool, PyBytes, PyBytesMethods, PyDict, PyList, PyListMethods,
            PyMapping, PyMappingMethods, PyModule, PyModuleMethods, PyString, PyTuple,
        },
    },
    std::{
        alloc::{self, Layout},
        iter,
        marker::PhantomData,
        mem, slice, str,
        sync::Once,
    },
    wit_dylib_ffi::{
        self as wit, Call, ExportFunction, Interpreter, List, Type, Wit, WitOption, WitResult,
    },
};

#[expect(
    unsafe_op_in_unsafe_fn,
    reason = "wit_bindgen::generate produces code that does this"
)]
mod bindings {
    wit_bindgen::generate!({
        world: "init",
        path: "../wit",
        generate_all,
    });

    use super::MyExports;

    export!(MyExports);
}

static WIT: OnceCell<Wit> = OnceCell::new();
static STUB_WASI: OnceCell<bool> = OnceCell::new();
static EXPORTS: OnceCell<Vec<Export>> = OnceCell::new();
static RESOURCES: OnceCell<Vec<Resource>> = OnceCell::new();
static RECORDS: OnceCell<Vec<Record>> = OnceCell::new();
static FLAGS: OnceCell<Vec<Flags>> = OnceCell::new();
static TUPLES: OnceCell<Vec<Tuple>> = OnceCell::new();
static VARIANTS: OnceCell<Vec<Variant>> = OnceCell::new();
static ENUMS: OnceCell<Vec<Enum>> = OnceCell::new();
static OPTIONS: OnceCell<Vec<OptionKind>> = OnceCell::new();
static RESULTS: OnceCell<Vec<ResultRecord>> = OnceCell::new();
static ENVIRON: OnceCell<Py<PyMapping>> = OnceCell::new();
static SOME_CONSTRUCTOR: OnceCell<Py<PyAny>> = OnceCell::new();
static OK_CONSTRUCTOR: OnceCell<Py<PyAny>> = OnceCell::new();
static ERR_CONSTRUCTOR: OnceCell<Py<PyAny>> = OnceCell::new();
static FINALIZE: OnceCell<Py<PyAny>> = OnceCell::new();
static DROP_RESOURCE: OnceCell<Py<PyAny>> = OnceCell::new();
static SEED: OnceCell<Py<PyAny>> = OnceCell::new();
static ARGV: OnceCell<Py<PyList>> = OnceCell::new();

struct Borrow {
    value: Py<PyAny>,
    handle: u32,
    drop: unsafe extern "C" fn(u32),
}

#[derive(Debug)]
struct Case {
    constructor: Py<PyAny>,
    has_payload: bool,
}

#[derive(Debug)]
struct Resource {
    constructor: Py<PyAny>,
}

#[derive(Debug)]
struct Record {
    constructor: Py<PyAny>,
    fields: Vec<String>,
}

#[derive(Debug)]
struct Flags {
    constructor: Py<PyAny>,
    u32_count: usize,
}

#[derive(Debug)]
struct Tuple {
    count: usize,
}

#[derive(Debug)]
struct Variant {
    types_to_discriminants: Py<PyDict>,
    cases: Vec<Case>,
}

#[derive(Debug)]
struct Enum {
    constructor: Py<PyAny>,
    count: usize,
}

#[derive(Debug)]
struct Export {
    kind: ExportKind,
    return_style: ReturnStyle,
}

#[derive(Debug)]
enum ExportKind {
    Freestanding {
        instance: Py<PyAny>,
        name: Py<PyString>,
    },
    Constructor(Py<PyAny>),
    Method(Py<PyString>),
    Static {
        class: Py<PyAny>,
        name: Py<PyString>,
    },
}

struct Anyhow(Error);

impl From<Anyhow> for PyErr {
    fn from(Anyhow(error): Anyhow) -> Self {
        PyAssertionError::new_err(format!("{error:?}"))
    }
}

impl<T: std::error::Error + Send + Sync + 'static> From<T> for Anyhow {
    fn from(error: T) -> Self {
        Self(error.into())
    }
}

fn release_borrows(py: Python, borrows: Vec<Borrow>) {
    for Borrow {
        value,
        handle,
        drop,
    } in borrows
    {
        let value = value.bind(py);

        value.delattr(intern!(py, "handle")).unwrap();

        value
            .getattr(intern!(py, "finalizer"))
            .unwrap()
            .call_method0(intern!(py, "detach"))
            .unwrap();

        unsafe {
            drop(handle);
        }
    }
}

#[pyo3::pyfunction]
#[pyo3(pass_module)]
fn call_import<'a>(
    module: Bound<'a, PyModule>,
    index: u32,
    params: Vec<Bound<'a, PyAny>>,
) -> Bound<'a, PyAny> {
    let py = module.py();
    let func = WIT
        .get()
        .unwrap()
        .import_func(usize::try_from(index).unwrap());

    let mut call = MyCall::new(params.into_iter().rev().map(|v| v.unbind()).collect());
    if func.is_async() {
        #[cfg(feature = "async")]
        {
            if let Some(pending) = unsafe { func.call_import_async(&mut call) } {
                ERR_CONSTRUCTOR
                    .get()
                    .unwrap()
                    .call1(
                        py,
                        (PyTuple::new(
                            py,
                            [
                                usize::try_from(pending.subtask).unwrap(),
                                Box::into_raw(Box::new(async_::Promise::ImportCall {
                                    index,
                                    call,
                                    buffer: pending.buffer,
                                })) as usize,
                            ],
                        )
                        .unwrap(),),
                    )
                    .unwrap()
            } else {
                assert!(call.stack.len() < 2);
                OK_CONSTRUCTOR
                    .get()
                    .unwrap()
                    .call1(py, (call.stack.pop().unwrap_or(py.None()),))
                    .unwrap()
            }
        }
        #[cfg(not(feature = "async"))]
        {
            panic!("async feature disabled")
        }
    } else {
        func.call_import_sync(&mut call);
        assert!(call.stack.len() < 2);
        call.stack.pop().unwrap_or(py.None())
    }
    .into_bound(py)
}

#[pyo3::pyfunction]
#[pyo3(pass_module)]
fn drop_resource(_module: &Bound<PyModule>, index: usize, handle: u32) -> PyResult<()> {
    unsafe {
        mem::transmute::<usize, unsafe extern "C" fn(u32)>(index)(handle);
    }
    Ok(())
}

#[cfg(feature = "async")]
mod async_ {
    use {super::*, pyo3::exceptions::PyMemoryError};

    const RETURN_CODE_BLOCKED: u32 = 0xFFFF_FFFF;
    const RETURN_CODE_COMPLETED: u32 = 0x0;
    const RETURN_CODE_DROPPED: u32 = 0x1;

    pub static CALLBACK: OnceCell<Py<PyAny>> = OnceCell::new();
    pub static BYTE_STREAM_READER_CONSTRUCTOR: OnceCell<Py<PyAny>> = OnceCell::new();
    pub static STREAM_READER_CONSTRUCTOR: OnceCell<Py<PyAny>> = OnceCell::new();
    pub static FUTURE_READER_CONSTRUCTOR: OnceCell<Py<PyAny>> = OnceCell::new();

    pub struct EmptyResource {
        pub value: Py<PyAny>,
        pub handle: u32,
        pub finalizer_args: Py<PyTuple>,
    }

    impl EmptyResource {
        fn restore(&self, py: Python) {
            self.value
                .setattr(py, intern!(py, "handle"), self.handle)
                .unwrap();

            let finalizer = FINALIZE
                .get()
                .unwrap()
                .call1(py, self.finalizer_args.clone_ref(py))
                .unwrap();

            self.value
                .setattr(py, intern!(py, "finalizer"), finalizer)
                .unwrap();
        }
    }

    pub enum Promise {
        ImportCall {
            index: u32,
            call: MyCall<'static>,
            buffer: *mut u8,
        },
        StreamRead {
            call: MyCall<'static>,
            ty: wit::Stream,
            buffer: *mut u8,
        },
        StreamWrite {
            _call: Option<MyCall<'static>>,
            _values: Option<Py<PyBytes>>,
            resources: Option<Vec<Vec<EmptyResource>>>,
        },
        FutureRead {
            call: MyCall<'static>,
            ty: wit::Future,
            buffer: *mut u8,
        },
        FutureWrite {
            _call: MyCall<'static>,
            resources: Option<Vec<EmptyResource>>,
        },
    }

    #[pyo3::pyfunction]
    #[pyo3(pass_module)]
    fn promise_get_result<'a>(
        module: Bound<'a, PyModule>,
        event: u32,
        promise: usize,
    ) -> Bound<'a, PyAny> {
        let py = module.py();
        let mut promise = unsafe { Box::from_raw(promise as *mut Promise) };

        match *promise.as_mut() {
            Promise::ImportCall {
                index,
                ref mut call,
                buffer,
            } => {
                let func = WIT
                    .get()
                    .unwrap()
                    .import_func(usize::try_from(index).unwrap());

                unsafe { func.lift_import_async_result(call, buffer) };
                assert!(call.stack.len() < 2);
                call.stack.pop().unwrap_or(py.None()).into_bound(py)
            }
            Promise::StreamRead {
                ref mut call,
                ty,
                buffer,
            } => {
                let count = usize::try_from(event >> 4).unwrap();
                let code = event & 0xF;
                PyTuple::new(
                    py,
                    [
                        code.into_pyobject(py).unwrap().into_any(),
                        if let Some(Type::U8 | Type::S8) = ty.ty() {
                            unsafe { PyBytes::from_ptr(py, buffer, count) }.into_any()
                        } else {
                            let list = PyList::empty(py);
                            for offset in 0..count {
                                unsafe {
                                    ty.lift(call, buffer.add(ty.abi_payload_size() * offset))
                                };
                                list.append(call.stack.pop().unwrap()).unwrap();
                            }
                            list.into_any()
                        },
                    ],
                )
                .unwrap()
                .into_any()
            }
            Promise::StreamWrite { ref resources, .. } => {
                let read_count = event >> 4;
                let code = event & 0xF;

                if let Some(resources) = resources {
                    for resources in &resources[usize::try_from(read_count).unwrap()..] {
                        for resource in resources {
                            resource.restore(py)
                        }
                    }
                }

                PyTuple::new(py, [code, read_count]).unwrap().into_any()
            }
            Promise::FutureRead {
                ref mut call,
                ty,
                buffer,
            } => {
                let code = event & 0xF;
                if let RETURN_CODE_COMPLETED | RETURN_CODE_DROPPED = code {
                    unsafe { ty.lift(call, buffer) }
                }
                call.stack.pop().unwrap_or(py.None()).into_bound(py)
            }
            Promise::FutureWrite { ref resources, .. } => {
                let count = event >> 4;
                let code = event & 0xF;

                if let (RETURN_CODE_DROPPED, Some(resources)) = (code, resources) {
                    for resource in resources {
                        resource.restore(py)
                    }
                }

                PyTuple::new(py, [code, count]).unwrap().into_any()
            }
        }
    }

    #[pyo3::pyfunction]
    fn waitable_set_new() -> u32 {
        #[link(wasm_import_module = "$root")]
        unsafe extern "C" {
            #[link_name = "[waitable-set-new]"]
            pub fn waitable_set_new() -> u32;
        }

        unsafe { waitable_set_new() }
    }

    #[pyo3::pyfunction]
    fn waitable_join(waitable: u32, set: u32) {
        #[link(wasm_import_module = "$root")]
        unsafe extern "C" {
            #[link_name = "[waitable-join]"]
            pub fn waitable_join(waitable: u32, set: u32);
        }

        unsafe { waitable_join(waitable, set) }
    }

    #[pyo3::pyfunction]
    fn context_set(value: Bound<PyAny>) {
        #[link(wasm_import_module = "$root")]
        unsafe extern "C" {
            #[link_name = "[context-set-0]"]
            pub fn context_set(value: u32);
        }

        unsafe {
            context_set(if value.is_none() {
                0
            } else {
                u32::try_from(value.into_ptr() as usize).unwrap()
            })
        }
    }

    #[pyo3::pyfunction]
    #[pyo3(pass_module)]
    fn context_get<'a>(module: Bound<'a, PyModule>) -> Bound<'a, PyAny> {
        #[link(wasm_import_module = "$root")]
        unsafe extern "C" {
            #[link_name = "[context-get-0]"]
            pub fn context_get() -> u32;
        }
        unsafe {
            let value = context_get();
            if value == 0 {
                module.py().None().into_bound(module.py())
            } else {
                Bound::from_owned_ptr(
                    module.py(),
                    usize::try_from(value).unwrap() as *mut pyo3::ffi::PyObject,
                )
            }
        }
    }

    #[pyo3::pyfunction]
    fn subtask_drop(task: u32) {
        #[link(wasm_import_module = "$root")]
        unsafe extern "C" {
            #[link_name = "[subtask-drop]"]
            pub fn subtask_drop(task: u32);
        }

        unsafe { subtask_drop(task) }
    }

    #[pyo3::pyfunction]
    fn waitable_set_drop(set: u32) {
        #[link(wasm_import_module = "$root")]
        unsafe extern "C" {
            #[link_name = "[waitable-set-drop]"]
            pub fn waitable_set_drop(set: u32);
        }

        unsafe { waitable_set_drop(set) }
    }

    #[pyo3::pyfunction]
    #[pyo3(pass_module)]
    fn call_task_return(module: Bound<PyModule>, index: u32, borrows: usize, result: Bound<PyAny>) {
        let py = module.py();
        let index = usize::try_from(index).unwrap();
        let func = WIT.get().unwrap().export_func(index);
        let export = &EXPORTS.get().unwrap()[index];
        let result = result.unbind();

        let results = match export.return_style {
            ReturnStyle::None => Vec::new(),
            ReturnStyle::Normal => vec![result.getattr(py, intern!(py, "value")).unwrap()],
            ReturnStyle::Result => vec![result],
        };

        let mut call = MyCall::new(results);
        func.call_task_return(&mut call);
        if borrows != 0 {
            release_borrows(py, *unsafe { Box::from_raw(borrows as *mut Vec<Borrow>) });
        }
    }

    #[pyo3::pyfunction]
    fn stream_new(ty: usize) -> u64 {
        unsafe { WIT.get().unwrap().stream(ty).new()() }
    }

    #[pyo3::pyfunction]
    #[pyo3(pass_module)]
    fn stream_read<'a>(
        module: Bound<'a, PyModule>,
        ty: usize,
        handle: u32,
        max_count: u32,
    ) -> PyResult<Bound<'a, PyAny>> {
        let py = module.py();
        let ty = WIT.get().unwrap().stream(ty);
        let mut call = MyCall::new(Vec::new());
        let max_count = usize::try_from(max_count).unwrap();
        let layout =
            Layout::from_size_align(ty.abi_payload_size() * max_count, ty.abi_payload_align())
                .unwrap();
        let buffer = unsafe { std::alloc::alloc(layout) };
        if buffer.is_null() {
            Err(PyMemoryError::new_err(
                "`stream.read` buffer allocation failed",
            ))
        } else {
            unsafe { call.defer_deallocate(buffer, layout) };

            let code = unsafe { ty.read()(handle, buffer.cast(), max_count) };

            Ok(if code == RETURN_CODE_BLOCKED {
                ERR_CONSTRUCTOR
                    .get()
                    .unwrap()
                    .call1(
                        py,
                        (PyTuple::new(
                            py,
                            [
                                usize::try_from(handle).unwrap(),
                                Box::into_raw(Box::new(async_::Promise::StreamRead {
                                    call,
                                    ty,
                                    buffer,
                                })) as usize,
                            ],
                        )
                        .unwrap(),),
                    )
                    .unwrap()
            } else {
                let count = usize::try_from(code >> 4).unwrap();
                let code = code & 0xF;
                OK_CONSTRUCTOR
                    .get()
                    .unwrap()
                    .call1(
                        py,
                        (PyTuple::new(
                            py,
                            [
                                code.into_pyobject(py).unwrap().into_any(),
                                if let Some(Type::U8 | Type::S8) = ty.ty() {
                                    unsafe { PyBytes::from_ptr(py, buffer, count) }.into_any()
                                } else {
                                    let list = PyList::empty(py);
                                    for offset in 0..count {
                                        unsafe {
                                            ty.lift(
                                                &mut call,
                                                buffer.add(ty.abi_payload_size() * offset),
                                            )
                                        };
                                        list.append(call.stack.pop().unwrap()).unwrap();
                                    }
                                    list.into_any()
                                },
                            ],
                        )
                        .unwrap(),),
                    )
                    .unwrap()
            }
            .into_bound(py))
        }
    }

    #[pyo3::pyfunction]
    #[pyo3(pass_module)]
    fn stream_write<'a>(
        module: Bound<'a, PyModule>,
        ty: usize,
        handle: u32,
        values: Bound<'a, PyAny>,
    ) -> PyResult<Bound<'a, PyAny>> {
        let py = module.py();
        let ty = WIT.get().unwrap().stream(ty);
        if let Some(Type::U8 | Type::S8) = ty.ty() {
            let values = values.cast_into::<PyBytes>().unwrap();
            let code = unsafe {
                ty.write()(
                    handle,
                    values.as_bytes().as_ptr().cast(),
                    values.len().unwrap(),
                )
            };

            Ok(if code == RETURN_CODE_BLOCKED {
                ERR_CONSTRUCTOR
                    .get()
                    .unwrap()
                    .call1(
                        py,
                        (PyTuple::new(
                            py,
                            [
                                usize::try_from(handle).unwrap(),
                                Box::into_raw(Box::new(async_::Promise::StreamWrite {
                                    _call: None,
                                    _values: Some(values.unbind()),
                                    resources: None,
                                })) as usize,
                            ],
                        )
                        .unwrap(),),
                    )
                    .unwrap()
            } else {
                let count = code >> 4;
                let code = code & 0xF;
                OK_CONSTRUCTOR
                    .get()
                    .unwrap()
                    .call1(py, (PyTuple::new(py, [code, count]).unwrap(),))
                    .unwrap()
            }
            .into_bound(py))
        } else {
            let values = values.cast_into::<PyList>().unwrap();
            let write_count = values.len();
            let mut call = MyCall::new(Vec::new());
            let layout = Layout::from_size_align(
                ty.abi_payload_size() * write_count,
                ty.abi_payload_align(),
            )
            .unwrap();
            let buffer = unsafe { std::alloc::alloc(layout) };
            if buffer.is_null() {
                Err(PyMemoryError::new_err(
                    "`future.write` buffer allocation failed",
                ))
            } else {
                unsafe { call.defer_deallocate(buffer, layout) };

                let mut resources = Vec::with_capacity(write_count);
                let mut need_restore_resources = false;
                for offset in 0..write_count {
                    call.stack.push(values.get_item(offset).unwrap().unbind());
                    call.resources = Some(Vec::new());
                    unsafe { ty.lower(&mut call, buffer.add(ty.abi_payload_size() * offset)) };
                    let res = call.resources.take().unwrap();
                    if !res.is_empty() {
                        need_restore_resources = true;
                    }
                    resources.push(res);
                }

                let code = unsafe { ty.write()(handle, buffer.cast(), write_count) };

                Ok(if code == RETURN_CODE_BLOCKED {
                    ERR_CONSTRUCTOR
                        .get()
                        .unwrap()
                        .call1(
                            py,
                            (PyTuple::new(
                                py,
                                [
                                    usize::try_from(handle).unwrap(),
                                    Box::into_raw(Box::new(async_::Promise::StreamWrite {
                                        _call: Some(call),
                                        _values: None,
                                        resources: need_restore_resources.then_some(resources),
                                    })) as usize,
                                ],
                            )
                            .unwrap(),),
                        )
                        .unwrap()
                } else {
                    let read_count = code >> 4;
                    let code = code & 0xF;

                    if need_restore_resources {
                        for resources in &resources[usize::try_from(read_count).unwrap()..] {
                            for resource in resources {
                                resource.restore(py)
                            }
                        }
                    }

                    OK_CONSTRUCTOR
                        .get()
                        .unwrap()
                        .call1(py, (PyTuple::new(py, [code, read_count]).unwrap(),))
                        .unwrap()
                }
                .into_bound(py))
            }
        }
    }

    #[pyo3::pyfunction]
    fn stream_drop_readable(ty: usize, handle: u32) {
        unsafe { WIT.get().unwrap().stream(ty).drop_readable()(handle) };
    }

    #[pyo3::pyfunction]
    fn stream_drop_writable(ty: usize, handle: u32) {
        unsafe { WIT.get().unwrap().stream(ty).drop_writable()(handle) };
    }

    #[pyo3::pyfunction]
    fn future_new(ty: usize) -> u64 {
        unsafe { WIT.get().unwrap().future(ty).new()() }
    }

    #[pyo3::pyfunction]
    #[pyo3(pass_module)]
    fn future_read<'a>(
        module: Bound<'a, PyModule>,
        ty: usize,
        handle: u32,
    ) -> PyResult<Bound<'a, PyAny>> {
        let py = module.py();
        let ty = WIT.get().unwrap().future(ty);
        let mut call = MyCall::new(Vec::new());
        let layout =
            Layout::from_size_align(ty.abi_payload_size(), ty.abi_payload_align()).unwrap();
        let buffer = unsafe { std::alloc::alloc(layout) };
        if buffer.is_null() {
            Err(PyMemoryError::new_err(
                "`future.read` buffer allocation failed",
            ))
        } else {
            unsafe { call.defer_deallocate(buffer, layout) };

            let code = unsafe { ty.read()(handle, buffer.cast()) };

            Ok(if code == RETURN_CODE_BLOCKED {
                ERR_CONSTRUCTOR
                    .get()
                    .unwrap()
                    .call1(
                        py,
                        (PyTuple::new(
                            py,
                            [
                                usize::try_from(handle).unwrap(),
                                Box::into_raw(Box::new(async_::Promise::FutureRead {
                                    call,
                                    ty,
                                    buffer,
                                })) as usize,
                            ],
                        )
                        .unwrap(),),
                    )
                    .unwrap()
            } else {
                let code = code & 0xF;
                if let RETURN_CODE_COMPLETED | RETURN_CODE_DROPPED = code {
                    unsafe { ty.lift(&mut call, buffer) }
                }
                OK_CONSTRUCTOR
                    .get()
                    .unwrap()
                    .call1(py, (call.stack.pop().unwrap_or(py.None()),))
                    .unwrap()
            }
            .into_bound(py))
        }
    }

    #[pyo3::pyfunction]
    #[pyo3(pass_module)]
    fn future_write<'a>(
        module: Bound<'a, PyModule>,
        ty: usize,
        handle: u32,
        value: Bound<'a, PyAny>,
    ) -> PyResult<Bound<'a, PyAny>> {
        let py = module.py();
        let ty = WIT.get().unwrap().future(ty);
        let mut call = MyCall::new(vec![value.unbind()]);
        let layout =
            Layout::from_size_align(ty.abi_payload_size(), ty.abi_payload_align()).unwrap();
        let buffer = unsafe { std::alloc::alloc(layout) };
        if buffer.is_null() {
            Err(PyMemoryError::new_err(
                "`future.write` buffer allocation failed",
            ))
        } else {
            unsafe { call.defer_deallocate(buffer, layout) };

            call.resources = Some(Vec::new());
            let code = unsafe {
                ty.lower(&mut call, buffer);

                ty.write()(handle, buffer.cast())
            };
            let resources = call
                .resources
                .take()
                .and_then(|v| if v.is_empty() { None } else { Some(v) });

            Ok(if code == RETURN_CODE_BLOCKED {
                ERR_CONSTRUCTOR
                    .get()
                    .unwrap()
                    .call1(
                        py,
                        (PyTuple::new(
                            py,
                            [
                                usize::try_from(handle).unwrap(),
                                Box::into_raw(Box::new(async_::Promise::FutureWrite {
                                    _call: call,
                                    resources,
                                })) as usize,
                            ],
                        )
                        .unwrap(),),
                    )
                    .unwrap()
            } else {
                let count = code >> 4;
                let code = code & 0xF;

                if let (RETURN_CODE_DROPPED, Some(resources)) = (code, &resources) {
                    for resource in resources {
                        resource.restore(py)
                    }
                }

                OK_CONSTRUCTOR
                    .get()
                    .unwrap()
                    .call1(py, (PyTuple::new(py, [code, count]).unwrap(),))
                    .unwrap()
            }
            .into_bound(py))
        }
    }

    #[pyo3::pyfunction]
    fn future_drop_readable(ty: usize, handle: u32) {
        unsafe { WIT.get().unwrap().future(ty).drop_readable()(handle) };
    }

    #[pyo3::pyfunction]
    fn future_drop_writable(ty: usize, handle: u32) {
        unsafe { WIT.get().unwrap().future(ty).drop_writable()(handle) };
    }

    pub fn add_functions(module: &Bound<PyModule>) -> PyResult<()> {
        module.add_function(pyo3::wrap_pyfunction!(promise_get_result, module)?)?;
        module.add_function(pyo3::wrap_pyfunction!(waitable_set_new, module)?)?;
        module.add_function(pyo3::wrap_pyfunction!(waitable_join, module)?)?;
        module.add_function(pyo3::wrap_pyfunction!(context_get, module)?)?;
        module.add_function(pyo3::wrap_pyfunction!(context_set, module)?)?;
        module.add_function(pyo3::wrap_pyfunction!(subtask_drop, module)?)?;
        module.add_function(pyo3::wrap_pyfunction!(waitable_set_drop, module)?)?;
        module.add_function(pyo3::wrap_pyfunction!(call_task_return, module)?)?;
        module.add_function(pyo3::wrap_pyfunction!(stream_new, module)?)?;
        module.add_function(pyo3::wrap_pyfunction!(stream_read, module)?)?;
        module.add_function(pyo3::wrap_pyfunction!(stream_write, module)?)?;
        module.add_function(pyo3::wrap_pyfunction!(stream_drop_readable, module)?)?;
        module.add_function(pyo3::wrap_pyfunction!(stream_drop_writable, module)?)?;
        module.add_function(pyo3::wrap_pyfunction!(future_new, module)?)?;
        module.add_function(pyo3::wrap_pyfunction!(future_read, module)?)?;
        module.add_function(pyo3::wrap_pyfunction!(future_write, module)?)?;
        module.add_function(pyo3::wrap_pyfunction!(future_drop_readable, module)?)?;
        module.add_function(pyo3::wrap_pyfunction!(future_drop_writable, module)?)
    }
}

#[pyo3::pymodule]
#[pyo3(name = "componentize_py_runtime")]
fn componentize_py_module(_py: Python<'_>, module: &Bound<PyModule>) -> PyResult<()> {
    module.add_function(pyo3::wrap_pyfunction!(call_import, module)?)?;
    module.add_function(pyo3::wrap_pyfunction!(drop_resource, module)?)?;

    #[cfg(feature = "async")]
    async_::add_functions(module)?;

    Ok(())
}

fn do_init(app_name: String, symbols: Symbols, stub_wasi: bool) -> Result<(), String> {
    pyo3::append_to_inittab!(componentize_py_module);

    Python::initialize();

    let init = |py: Python| {
        let app = py.import(app_name.as_str())?;

        STUB_WASI.set(stub_wasi).unwrap();

        EXPORTS
            .set(
                symbols
                    .exports
                    .iter()
                    .map(|export| {
                        Ok(Export {
                            kind: match &export.kind {
                                FunctionExportKind::Freestanding(exp::Function {
                                    protocol,
                                    name,
                                }) => ExportKind::Freestanding {
                                    name: PyString::intern(py, name).into(),
                                    instance: app.getattr(protocol.as_str())?.call0()?.into(),
                                },
                                FunctionExportKind::Constructor(Constructor {
                                    module,
                                    protocol,
                                }) => ExportKind::Constructor(
                                    py.import(module.as_str())?
                                        .getattr(protocol.as_str())?
                                        .into(),
                                ),
                                FunctionExportKind::Method(name) => {
                                    ExportKind::Method(PyString::intern(py, name).into())
                                }
                                FunctionExportKind::Static(Static {
                                    module,
                                    protocol,
                                    name,
                                }) => ExportKind::Static {
                                    name: PyString::intern(py, name).into(),
                                    class: py
                                        .import(module.as_str())?
                                        .getattr(protocol.as_str())?
                                        .into(),
                                },
                            },
                            return_style: export.return_style,
                        })
                    })
                    .collect::<PyResult<_>>()?,
            )
            .unwrap();

        RESOURCES
            .set(
                symbols
                    .resources
                    .into_iter()
                    .map(|ty| {
                        Ok(Resource {
                            constructor: py
                                .import(ty.package.as_str())?
                                .getattr(ty.name.as_str())?
                                .into(),
                        })
                    })
                    .collect::<PyResult<_>>()?,
            )
            .unwrap();

        RECORDS
            .set(
                symbols
                    .records
                    .into_iter()
                    .map(|ty| {
                        Ok(Record {
                            constructor: py
                                .import(ty.package.as_str())?
                                .getattr(ty.name.as_str())?
                                .into(),
                            fields: ty.fields,
                        })
                    })
                    .collect::<PyResult<_>>()?,
            )
            .unwrap();

        FLAGS
            .set(
                symbols
                    .flags
                    .into_iter()
                    .map(|ty| {
                        Ok(Flags {
                            constructor: py
                                .import(ty.package.as_str())?
                                .getattr(ty.name.as_str())?
                                .into(),
                            u32_count: ty.u32_count.try_into().unwrap(),
                        })
                    })
                    .collect::<PyResult<_>>()?,
            )
            .unwrap();

        TUPLES
            .set(
                symbols
                    .tuples
                    .into_iter()
                    .map(|ty| {
                        Ok(Tuple {
                            count: ty.count.try_into().unwrap(),
                        })
                    })
                    .collect::<PyResult<_>>()?,
            )
            .unwrap();

        VARIANTS
            .set(
                symbols
                    .variants
                    .into_iter()
                    .map(|ty| {
                        let package = py.import(ty.package.as_str())?;

                        let cases = ty
                            .cases
                            .iter()
                            .map(|case| {
                                Ok(Case {
                                    constructor: package.getattr(case.name.as_str())?.into(),
                                    has_payload: case.has_payload,
                                })
                            })
                            .collect::<PyResult<Vec<_>>>()?;

                        let types_to_discriminants = PyDict::new(py);
                        for (index, case) in cases.iter().enumerate() {
                            types_to_discriminants.set_item(&case.constructor, index)?;
                        }

                        Ok(Variant {
                            cases,
                            types_to_discriminants: types_to_discriminants.into(),
                        })
                    })
                    .collect::<PyResult<_>>()?,
            )
            .unwrap();

        ENUMS
            .set(
                symbols
                    .enums
                    .into_iter()
                    .map(|ty| {
                        Ok(Enum {
                            constructor: py
                                .import(ty.package.as_str())?
                                .getattr(ty.name.as_str())?
                                .into(),
                            count: ty.count.try_into().unwrap(),
                        })
                    })
                    .collect::<PyResult<_>>()?,
            )
            .unwrap();

        OPTIONS.set(symbols.options).unwrap();

        RESULTS.set(symbols.results).unwrap();

        let types = py.import("componentize_py_types")?;

        SOME_CONSTRUCTOR.set(types.getattr("Some")?.into()).unwrap();
        OK_CONSTRUCTOR.set(types.getattr("Ok")?.into()).unwrap();
        ERR_CONSTRUCTOR.set(types.getattr("Err")?.into()).unwrap();

        let environ = py
            .import("os")?
            .getattr("environ")?
            .downcast_into::<PyMapping>()
            .unwrap();

        let keys = environ.keys()?;

        for i in 0..keys.len() {
            environ.del_item(keys.get_item(i)?)?;
        }

        ENVIRON.set(environ.into()).unwrap();

        FINALIZE
            .set(py.import("weakref")?.getattr("finalize")?.into())
            .unwrap();

        DROP_RESOURCE
            .set(
                py.import("componentize_py_runtime")?
                    .getattr("drop_resource")?
                    .into(),
            )
            .unwrap();

        SEED.set(py.import("random")?.getattr("seed")?.into())
            .unwrap();

        #[cfg(feature = "async")]
        {
            async_::CALLBACK
                .set(
                    py.import("componentize_py_async_support")
                        .unwrap()
                        .getattr("callback")
                        .unwrap()
                        .into(),
                )
                .unwrap();

            let streams = py.import("componentize_py_async_support.streams").unwrap();

            async_::BYTE_STREAM_READER_CONSTRUCTOR
                .set(streams.getattr("ByteStreamReader").unwrap().into())
                .unwrap();

            async_::STREAM_READER_CONSTRUCTOR
                .set(streams.getattr("StreamReader").unwrap().into())
                .unwrap();

            async_::FUTURE_READER_CONSTRUCTOR
                .set(
                    py.import("componentize_py_async_support.futures")
                        .unwrap()
                        .getattr("FutureReader")
                        .unwrap()
                        .into(),
                )
                .unwrap();
        }

        let argv = py
            .import("sys")?
            .getattr("argv")?
            .downcast_into::<PyList>()
            .unwrap();

        for i in 0..argv.len() {
            argv.del_item(i)?;
        }

        ARGV.set(argv.into()).unwrap();

        Ok::<_, Error>(())
    };

    Python::attach(|py| {
        init(py).map_err(|e| {
            if let Some(e) = e.downcast_ref::<PyErr>() {
                e.print(py);
            }
            format!("{e:?}")
        })
    })
}

struct MyExports;

impl Guest for MyExports {
    fn init(app_name: String, symbols: Symbols, stub_wasi: bool) -> Result<(), String> {
        let result = do_init(app_name, symbols, stub_wasi);

        // This tells the WASI Preview 1 component adapter to reset its state.
        // In particular, we want it to forget about any open handles and
        // re-request the stdio handles at runtime since we'll be running under
        // a brand new host.
        #[link(wasm_import_module = "wasi_snapshot_preview1")]
        unsafe extern "C" {
            #[link_name = "reset_adapter_state"]
            fn reset_adapter_state();
        }

        // This tells wasi-libc to reset its preopen state, forcing
        // re-initialization at runtime.
        #[link(wasm_import_module = "env")]
        unsafe extern "C" {
            #[link_name = "__wasilibc_reset_preopens"]
            fn wasilibc_reset_preopens();
        }

        unsafe {
            reset_adapter_state();
            wasilibc_reset_preopens();
        }

        result
    }
}

struct MyInterpreter;

impl MyInterpreter {
    fn export_call_(func: ExportFunction, cx: &mut MyCall<'_>, async_: bool) -> u32 {
        Python::attach(|py| {
            if !*STUB_WASI.get().unwrap() {
                static ONCE: Once = Once::new();
                ONCE.call_once(|| {
                    // We must call directly into the host to get the runtime
                    // environment since libc's version will only contain the
                    // build-time pre-init snapshot.
                    let environ = ENVIRON.get().unwrap().bind(py);
                    for (k, v) in environment::get_environment() {
                        environ.set_item(k, v).unwrap();
                    }

                    // Likewise for CLI arguments.
                    for arg in environment::get_arguments() {
                        ARGV.get().unwrap().bind(py).append(arg).unwrap();
                    }

                    // Call `random.seed()` to ensure we get a fresh seed rather
                    // than the one that got baked in during pre-init.
                    SEED.get().unwrap().call0(py).unwrap();
                });
            }

            if async_ {
                cx.stack.push(
                    if cx.borrows.is_empty() {
                        0
                    } else {
                        Box::into_raw(Box::new(mem::take(&mut cx.borrows))) as usize
                    }
                    .into_pyobject(py)
                    .unwrap()
                    .into_any()
                    .unbind(),
                );
            }

            let mut params_py = mem::take(&mut cx.stack).into_iter();
            let export = &EXPORTS.get().unwrap()[func.index()];
            let result = match &export.kind {
                ExportKind::Freestanding { instance, name } => {
                    instance.call_method1(py, name, PyTuple::new(py, params_py).unwrap())
                }
                ExportKind::Constructor(class) => {
                    class.call1(py, PyTuple::new(py, params_py).unwrap())
                }
                ExportKind::Method(name) => params_py
                    // Call method on self with remaining iterator elements
                    .next()
                    .unwrap()
                    .call_method1(py, name, PyTuple::new(py, params_py).unwrap())
                    .map(|r| r.into()),
                ExportKind::Static { class, name } => class
                    .getattr(py, name)
                    .and_then(|function| function.call1(py, PyTuple::new(py, params_py).unwrap())),
            };

            if async_ {
                match result {
                    Ok(result) => result.extract(py).unwrap(),
                    Err(error) => {
                        error.print(py);
                        panic!("Python function threw an unexpected exception")
                    }
                }
            } else {
                let result = match (result, export.return_style) {
                    (Ok(_), ReturnStyle::None) => None,
                    (Ok(result), ReturnStyle::Normal) => Some(result),
                    (Ok(result), ReturnStyle::Result) => {
                        Some(OK_CONSTRUCTOR.get().unwrap().call1(py, (result,)).unwrap())
                    }
                    (Err(error), ReturnStyle::None | ReturnStyle::Normal) => {
                        error.print(py);
                        panic!("Python function threw an unexpected exception")
                    }
                    (Err(error), ReturnStyle::Result) => {
                        if ERR_CONSTRUCTOR
                            .get()
                            .unwrap()
                            .bind(py)
                            .eq(error.get_type(py))
                            .unwrap()
                        {
                            Some(error.into_value(py).into_any())
                        } else {
                            error.print(py);
                            panic!("Python function threw an unexpected exception")
                        }
                    }
                };

                if let Some(result) = result {
                    cx.stack.push(result);
                }

                release_borrows(py, mem::take(&mut cx.borrows));

                0
            }
        })
    }
}

impl Interpreter for MyInterpreter {
    type CallCx<'a> = MyCall<'a>;

    fn initialize(wit: Wit) {
        WIT.set(wit).map_err(drop).unwrap();
    }

    fn export_start<'a>(_: Wit, _: ExportFunction) -> Box<MyCall<'a>> {
        Box::new(MyCall::new(Vec::new()))
    }

    fn export_call(_: Wit, func: ExportFunction, cx: &mut MyCall<'_>) {
        Self::export_call_(func, cx, false);
    }

    fn export_async_start(_: Wit, func: ExportFunction, mut cx: Box<MyCall<'_>>) -> u32 {
        #[cfg(feature = "async")]
        {
            Self::export_call_(func, &mut cx, true)
        }
        #[cfg(not(feature = "async"))]
        {
            _ = (func, &mut cx);
            panic!("async feature disabled")
        }
    }

    fn export_async_callback(event0: u32, event1: u32, event2: u32) -> u32 {
        #[cfg(feature = "async")]
        {
            Python::attach(|py| {
                async_::CALLBACK
                    .get()
                    .unwrap()
                    .call1(py, (event0, event1, event2))
                    .unwrap()
                    .extract(py)
                    .unwrap()
            })
        }
        #[cfg(not(feature = "async"))]
        {
            _ = (event0, event1, event2);
            panic!("async feature disabled")
        }
    }

    fn resource_dtor(ty: wit::Resource, handle: usize) {
        // We don't currently include a `drop` function as part of the abstract
        // base class we generate for an exported resource, so there's nothing
        // to do here.  If/when that changes, we'll want to call `drop` here.
        _ = (ty, handle);
    }
}

struct MyCall<'a> {
    _phantom: PhantomData<&'a ()>,
    iter_stack: Vec<usize>,
    deferred_deallocations: Vec<(*mut u8, Layout)>,
    strings: Vec<String>,
    borrows: Vec<Borrow>,
    stack: Vec<Py<PyAny>>,
    #[cfg(feature = "async")]
    resources: Option<Vec<async_::EmptyResource>>,
}

impl MyCall<'_> {
    fn new(stack: Vec<Py<PyAny>>) -> Self {
        Self {
            _phantom: PhantomData,
            iter_stack: Vec::new(),
            deferred_deallocations: Vec::new(),
            strings: Vec::new(),
            borrows: Vec::new(),
            stack,
            #[cfg(feature = "async")]
            resources: None,
        }
    }

    fn imported_resource_to_canon(&mut self, py: Python<'_>, value: Py<PyAny>, owned: bool) -> u32 {
        let handle = value
            .bind(py)
            .getattr(intern!(py, "handle"))
            .unwrap()
            .extract()
            .unwrap();

        if owned {
            value.bind(py).delattr(intern!(py, "handle")).unwrap();

            let finalizer_args = value
                .bind(py)
                .getattr(intern!(py, "finalizer"))
                .unwrap()
                .call_method0(intern!(py, "detach"))
                .unwrap();

            #[cfg(feature = "async")]
            {
                if let Some(resources) = &mut self.resources {
                    resources.push(async_::EmptyResource {
                        value,
                        handle,
                        finalizer_args: finalizer_args.cast_into().unwrap().unbind(),
                    });
                }
            }
            #[cfg(not(feature = "async"))]
            {
                _ = finalizer_args;
            }
        }

        handle
    }
}

impl Drop for MyCall<'_> {
    fn drop(&mut self) {
        for &(ptr, layout) in &self.deferred_deallocations {
            unsafe {
                alloc::dealloc(ptr, layout);
            }
        }
    }
}

impl Call for MyCall<'_> {
    unsafe fn defer_deallocate(&mut self, ptr: *mut u8, layout: Layout) {
        self.deferred_deallocations.push((ptr, layout));
    }

    fn pop_u8(&mut self) -> u8 {
        Python::attach(|py| self.stack.pop().unwrap().extract(py).unwrap())
    }

    fn pop_u16(&mut self) -> u16 {
        Python::attach(|py| self.stack.pop().unwrap().extract(py).unwrap())
    }

    fn pop_u32(&mut self) -> u32 {
        Python::attach(|py| self.stack.pop().unwrap().extract(py).unwrap())
    }

    fn pop_u64(&mut self) -> u64 {
        Python::attach(|py| self.stack.pop().unwrap().extract(py).unwrap())
    }

    fn pop_s8(&mut self) -> i8 {
        Python::attach(|py| self.stack.pop().unwrap().extract(py).unwrap())
    }

    fn pop_s16(&mut self) -> i16 {
        Python::attach(|py| self.stack.pop().unwrap().extract(py).unwrap())
    }

    fn pop_s32(&mut self) -> i32 {
        Python::attach(|py| self.stack.pop().unwrap().extract(py).unwrap())
    }

    fn pop_s64(&mut self) -> i64 {
        Python::attach(|py| self.stack.pop().unwrap().extract(py).unwrap())
    }

    fn pop_bool(&mut self) -> bool {
        Python::attach(|py| self.stack.pop().unwrap().is_truthy(py).unwrap())
    }

    fn pop_char(&mut self) -> char {
        let value = Python::attach(|py| self.stack.pop().unwrap().extract::<String>(py).unwrap());
        assert!(value.chars().count() == 1);
        value.chars().next().unwrap()
    }

    fn pop_f32(&mut self) -> f32 {
        Python::attach(|py| self.stack.pop().unwrap().extract(py).unwrap())
    }

    fn pop_f64(&mut self) -> f64 {
        Python::attach(|py| self.stack.pop().unwrap().extract(py).unwrap())
    }

    fn pop_string(&mut self) -> &str {
        let value = Python::attach(|py| self.stack.pop().unwrap().extract::<String>(py).unwrap());
        self.strings.push(value);
        self.strings.last().unwrap()
    }

    fn pop_borrow(&mut self, ty: wit::Resource) -> u32 {
        Python::attach(|py| {
            let value = self.stack.pop().unwrap();
            if let Some(new) = ty.new() {
                // exported resource type
                exported_resource_to_canon(py, ty, new, value)
            } else {
                // imported resource type
                self.imported_resource_to_canon(py, value, false)
            }
        })
    }

    fn pop_own(&mut self, ty: wit::Resource) -> u32 {
        Python::attach(|py| {
            let value = self.stack.pop().unwrap();
            if let Some(new) = ty.new() {
                // exported resource type
                exported_resource_to_canon(py, ty, new, value)
            } else {
                // imported resource type
                self.imported_resource_to_canon(py, value, true)
            }
        })
    }

    fn pop_enum(&mut self, _ty: wit::Enum) -> u32 {
        Python::attach(|py| {
            let value = self.stack.pop().unwrap();
            let value = value.bind(py);
            value
                .getattr(intern!(py, "value"))
                .unwrap()
                .extract()
                .unwrap()
        })
    }

    fn pop_flags(&mut self, _ty: wit::Flags) -> u32 {
        Python::attach(|py| {
            let value = self.stack.pop().unwrap();
            let value = value.bind(py);
            // See the comment in `Self::push_flags` about using `num-bigint`
            // here to represent arbitrary bit lengths.
            value
                .getattr(intern!(py, "value"))
                .unwrap()
                .extract::<BigUint>()
                .unwrap()
                .iter_u32_digits()
                .next()
                .unwrap_or(0)
        })
    }

    fn pop_future(&mut self, _ty: wit::Future) -> u32 {
        Python::attach(|py| {
            let value = self.stack.pop().unwrap();

            self.imported_resource_to_canon(py, value, true)
        })
    }

    fn pop_stream(&mut self, _ty: wit::Stream) -> u32 {
        Python::attach(|py| {
            let value = self.stack.pop().unwrap();

            self.imported_resource_to_canon(py, value, true)
        })
    }

    fn pop_option(&mut self, ty: WitOption) -> u32 {
        Python::attach(|py| {
            if self.stack.last().unwrap().is_none(py) {
                self.stack.pop().unwrap();
                0
            } else {
                match &OPTIONS.get().unwrap()[ty.index()] {
                    OptionKind::NonNesting => {
                        // Leave value on the stack as-is
                    }
                    OptionKind::Nesting => {
                        let value = self.stack.pop().unwrap();
                        self.stack
                            .push(value.getattr(py, intern!(py, "value")).unwrap());
                    }
                }
                1
            }
        })
    }

    fn pop_result(&mut self, ty: WitResult) -> u32 {
        let &ResultRecord { has_ok, has_err } = &RESULTS.get().unwrap()[ty.index()];

        Python::attach(|py| {
            let value = self.stack.pop().unwrap();
            let value = value.bind(py);

            let (discriminant, has_payload) = if OK_CONSTRUCTOR
                .get()
                .unwrap()
                .bind(py)
                .eq(value.get_type())
                .unwrap()
            {
                (0, has_ok)
            } else if ERR_CONSTRUCTOR
                .get()
                .unwrap()
                .bind(py)
                .eq(value.get_type())
                .unwrap()
            {
                (1, has_err)
            } else {
                unreachable!()
            };

            if has_payload {
                self.stack
                    .push(value.getattr(intern!(py, "value")).unwrap().unbind());
            }

            discriminant
        })
    }

    fn pop_variant(&mut self, ty: wit::Variant) -> u32 {
        let Variant {
            types_to_discriminants,
            cases,
        } = &VARIANTS.get().unwrap()[ty.index()];

        Python::attach(|py| {
            let value = self.stack.pop().unwrap();
            let value = value.bind(py);

            let discriminant = types_to_discriminants
                .bind(py)
                .get_item(value.get_type())
                .unwrap()
                .extract::<usize>()
                .unwrap();

            if cases[discriminant].has_payload {
                self.stack
                    .push(value.getattr(intern!(py, "value")).unwrap().unbind())
            }

            u32::try_from(discriminant).unwrap()
        })
    }

    fn pop_record(&mut self, ty: wit::Record) {
        let Record { fields, .. } = &RECORDS.get().unwrap()[ty.index()];

        Python::attach(|py| {
            let value = self.stack.pop().unwrap();
            self.stack.extend(
                fields
                    .iter()
                    .rev()
                    .map(|name| value.getattr(py, name.as_str()).unwrap()),
            );
        });
    }

    fn pop_tuple(&mut self, ty: wit::Tuple) {
        let &Tuple { count } = &TUPLES.get().unwrap()[ty.index()];

        Python::attach(|py| {
            let value = self.stack.pop().unwrap();
            let value = value.cast_bound::<PyTuple>(py).unwrap();

            self.stack.extend(
                (0..count)
                    .rev()
                    .map(|index| value.get_item(index).unwrap().unbind()),
            );
        });
    }

    unsafe fn maybe_pop_list(&mut self, ty: List) -> Option<(*const u8, usize)> {
        if let Type::U8 | Type::S8 = ty.ty() {
            Python::attach(|py| {
                let src = self.stack.pop().unwrap();
                let src = src.cast_bound::<PyBytes>(py).unwrap();
                let len = src.len().unwrap();
                let dst = unsafe {
                    let dst = alloc::alloc(Layout::from_size_align(len, 1).unwrap());
                    slice::from_raw_parts_mut(dst, len).copy_from_slice(src.as_bytes());
                    dst
                };
                Some((dst as _, len))
            })
        } else {
            None
        }
    }

    fn pop_list(&mut self, _ty: List) -> usize {
        Python::attach(|py| {
            self.iter_stack.push(0);
            let value = self.stack.last().unwrap();
            value.cast_bound::<PyList>(py).unwrap().len()
        })
    }

    fn pop_iter_next(&mut self, _ty: List) {
        Python::attach(|py| {
            let index = *self.iter_stack.last().unwrap();
            let element = self
                .stack
                .last()
                .unwrap()
                .cast_bound::<PyList>(py)
                .unwrap()
                .get_item(index)
                .unwrap();
            *self.iter_stack.last_mut().unwrap() = index + 1;
            self.stack.push(element.into_any().unbind());
        })
    }

    fn pop_iter(&mut self, _ty: List) {
        self.iter_stack.pop().unwrap();
        Python::attach(|py| {
            self.stack.pop().unwrap().drop_ref(py);
        })
    }

    fn push_bool(&mut self, val: bool) {
        self.stack.push(Python::attach(|py| {
            PyBool::new(py, val).to_owned().into_any().unbind()
        }))
    }

    fn push_char(&mut self, val: char) {
        self.stack.push(Python::attach(|py| {
            val.to_string()
                .into_pyobject(py)
                .unwrap()
                .into_any()
                .unbind()
        }))
    }

    fn push_u8(&mut self, val: u8) {
        self.stack.push(Python::attach(|py| {
            val.into_pyobject(py).unwrap().into_any().unbind()
        }))
    }

    fn push_s8(&mut self, val: i8) {
        self.stack.push(Python::attach(|py| {
            val.into_pyobject(py).unwrap().into_any().unbind()
        }))
    }

    fn push_u16(&mut self, val: u16) {
        self.stack.push(Python::attach(|py| {
            val.into_pyobject(py).unwrap().into_any().unbind()
        }))
    }

    fn push_s16(&mut self, val: i16) {
        self.stack.push(Python::attach(|py| {
            val.into_pyobject(py).unwrap().into_any().unbind()
        }))
    }

    fn push_u32(&mut self, val: u32) {
        self.stack.push(Python::attach(|py| {
            val.into_pyobject(py).unwrap().into_any().unbind()
        }));
    }

    fn push_s32(&mut self, val: i32) {
        self.stack.push(Python::attach(|py| {
            val.into_pyobject(py).unwrap().into_any().unbind()
        }))
    }

    fn push_u64(&mut self, val: u64) {
        self.stack.push(Python::attach(|py| {
            val.into_pyobject(py).unwrap().into_any().unbind()
        }))
    }

    fn push_s64(&mut self, val: i64) {
        self.stack.push(Python::attach(|py| {
            val.into_pyobject(py).unwrap().into_any().unbind()
        }))
    }

    fn push_f32(&mut self, val: f32) {
        self.stack.push(Python::attach(|py| {
            val.into_pyobject(py).unwrap().into_any().unbind()
        }))
    }

    fn push_f64(&mut self, val: f64) {
        self.stack.push(Python::attach(|py| {
            val.into_pyobject(py).unwrap().into_any().unbind()
        }))
    }

    fn push_string(&mut self, val: String) {
        self.stack.push(Python::attach(|py| {
            val.into_pyobject(py).unwrap().into_any().unbind()
        }))
    }

    fn push_record(&mut self, ty: wit::Record) {
        let Record {
            constructor,
            fields,
        } = &RECORDS.get().unwrap()[ty.index()];

        let elements = self
            .stack
            .drain(self.stack.len().checked_sub(fields.len()).unwrap()..);

        let result = Python::attach(|py| {
            constructor
                .call1(py, PyTuple::new(py, elements).unwrap())
                .unwrap()
        });

        self.stack.push(result);
    }

    fn push_tuple(&mut self, ty: wit::Tuple) {
        let &Tuple { count } = &TUPLES.get().unwrap()[ty.index()];

        let elements = self
            .stack
            .drain(self.stack.len().checked_sub(count).unwrap()..);

        let result = Python::attach(|py| {
            PyTuple::new(py, elements)
                .unwrap()
                .to_owned()
                .into_any()
                .unbind()
        });

        self.stack.push(result);
    }

    fn push_flags(&mut self, ty: wit::Flags, bits: u32) {
        let Flags {
            constructor,
            u32_count,
        } = &FLAGS.get().unwrap()[ty.index()];

        // Note that `componentize-py` was originally written when a component
        // model `flags` type could have an arbitrary number of bits.  Since
        // then, the spec was updated to only allow up to 32 bits, but we still
        // support arbitrary sizes here.  See
        // https://github.com/WebAssembly/component-model/issues/370 for
        // details.
        //
        // TODO: If it's unlikely that the spec will ever support more than 32
        // bits, remove the `num-bigint` dependency and simplify the code here
        // and in `summary.rs`.
        let result = Python::attach(|py| {
            constructor
                .call1(
                    py,
                    (BigUint::new(
                        iter::once(bits)
                            .chain(iter::repeat(0))
                            .take(*u32_count)
                            .collect(),
                    ),),
                )
                .unwrap()
        });

        self.stack.push(result);
    }

    fn push_enum(&mut self, ty: wit::Enum, discriminant: u32) {
        let &Enum {
            ref constructor,
            count,
        } = &ENUMS.get().unwrap()[ty.index()];

        assert!(usize::try_from(discriminant).unwrap() < count);

        let result = Python::attach(|py| constructor.call1(py, (discriminant,)).unwrap());

        self.stack.push(result);
    }

    fn push_borrow(&mut self, ty: wit::Resource, handle: u32) {
        Python::attach(|py| {
            self.stack.push(if ty.rep().is_some() {
                // exported resource type
                unsafe { Py::<PyAny>::from_borrowed_ptr(py, handle as usize as _) }
            } else {
                // imported resource type
                let value = imported_resource_from_canon(py, ty, handle);

                self.borrows.push(Borrow {
                    value: value.clone_ref(py),
                    handle,
                    drop: ty.drop(),
                });

                value
            })
        })
    }

    fn push_own(&mut self, ty: wit::Resource, handle: u32) {
        Python::attach(|py| {
            self.stack.push(if let Some(rep) = ty.rep() {
                // exported resource type
                let rep = unsafe { rep(handle) };
                let value = unsafe { Py::<PyAny>::from_borrowed_ptr(py, rep as _) }.into_bound(py);

                value
                    .delattr(intern!(py, "__componentize_py_handle"))
                    .unwrap();

                value
                    .getattr(intern!(py, "finalizer"))
                    .unwrap()
                    .call_method0(intern!(py, "detach"))
                    .unwrap();

                value.unbind()
            } else {
                // imported resource type
                imported_resource_from_canon(py, ty, handle)
            });
        })
    }

    fn push_future(&mut self, ty: wit::Future, handle: u32) {
        #[cfg(feature = "async")]
        {
            let result = Python::attach(|py| {
                async_::FUTURE_READER_CONSTRUCTOR
                    .get()
                    .unwrap()
                    .call1(py, (ty.index(), handle))
                    .unwrap()
            });

            self.stack.push(result);
        }
        #[cfg(not(feature = "async"))]
        {
            _ = (ty, handle);
            panic!("async feature disabled")
        }
    }

    fn push_stream(&mut self, ty: wit::Stream, handle: u32) {
        #[cfg(feature = "async")]
        {
            let result = Python::attach(|py| {
                if let Some(Type::U8 | Type::S8) = ty.ty() {
                    async_::BYTE_STREAM_READER_CONSTRUCTOR.get()
                } else {
                    async_::STREAM_READER_CONSTRUCTOR.get()
                }
                .unwrap()
                .call1(py, (ty.index(), handle))
                .unwrap()
            });

            self.stack.push(result);
        }
        #[cfg(not(feature = "async"))]
        {
            _ = (ty, handle);
            panic!("async feature disabled")
        }
    }

    fn push_variant(&mut self, ty: wit::Variant, discriminant: u32) {
        let Variant { cases, .. } = &VARIANTS.get().unwrap()[ty.index()];

        let result = Python::attach(|py| {
            let case = &cases[usize::try_from(discriminant).unwrap()];
            if case.has_payload {
                let payload = self.stack.pop().unwrap();
                case.constructor.call1(py, (payload,))
            } else {
                case.constructor.call1(py, ())
            }
            .unwrap()
        });

        self.stack.push(result);
    }

    fn push_option(&mut self, ty: WitOption, is_some: bool) {
        Python::attach(|py| {
            if is_some {
                match &OPTIONS.get().unwrap()[ty.index()] {
                    OptionKind::NonNesting => {
                        // Leave payload on the stack as-is.
                    }
                    OptionKind::Nesting => {
                        let payload = self.stack.pop().unwrap();
                        self.stack.push(
                            SOME_CONSTRUCTOR
                                .get()
                                .unwrap()
                                .call1(py, (payload,))
                                .unwrap(),
                        );
                    }
                }
            } else {
                self.stack.push(py.None());
            }
        });
    }

    fn push_result(&mut self, ty: WitResult, is_err: bool) {
        let &ResultRecord { has_ok, has_err } = &RESULTS.get().unwrap()[ty.index()];

        Python::attach(|py| {
            let (constructor, has_payload) = if is_err {
                (ERR_CONSTRUCTOR.get().unwrap(), has_err)
            } else {
                (OK_CONSTRUCTOR.get().unwrap(), has_ok)
            };

            let payload = if has_payload {
                self.stack.pop().unwrap()
            } else {
                py.None()
            };

            self.stack.push(constructor.call1(py, (payload,)).unwrap())
        });
    }

    unsafe fn push_raw_list(&mut self, ty: List, src: *mut u8, len: usize) -> bool {
        if let Type::U8 | Type::S8 = ty.ty() {
            self.stack.push(Python::attach(|py| unsafe {
                let value = PyBytes::new(py, slice::from_raw_parts(src, len))
                    .to_owned()
                    .into_any()
                    .unbind();
                alloc::dealloc(src, Layout::from_size_align(len, 1).unwrap());
                value
            }));
            true
        } else {
            false
        }
    }

    fn push_list(&mut self, _ty: List, _capacity: usize) {
        self.stack.push(Python::attach(|py| {
            PyList::empty(py).to_owned().into_any().unbind()
        }));
    }

    fn list_append(&mut self, _ty: List) {
        Python::attach(|py| {
            let element = self.stack.pop().unwrap();
            self.stack
                .last()
                .unwrap()
                .cast_bound::<PyList>(py)
                .unwrap()
                .append(element)
                .unwrap()
        });
    }
}

fn imported_resource_from_canon(py: Python<'_>, ty: wit::Resource, handle: u32) -> Py<PyAny> {
    let Resource { constructor } = &RESOURCES.get().unwrap()[ty.index()];

    let instance = constructor
        .call_method1(
            py,
            intern!(py, "__new__"),
            PyTuple::new(py, [constructor]).unwrap(),
        )
        .unwrap();

    let handle = handle.into_pyobject(py).unwrap();

    instance
        .setattr(py, intern!(py, "handle"), handle.as_borrowed())
        .unwrap();

    let finalizer = FINALIZE
        .get()
        .unwrap()
        .call1(
            py,
            (
                instance.clone_ref(py),
                DROP_RESOURCE.get().unwrap(),
                (ty.drop() as usize).into_pyobject(py).unwrap(),
                handle,
            ),
        )
        .unwrap();

    instance
        .setattr(py, intern!(py, "finalizer"), finalizer)
        .unwrap();

    instance
}

fn exported_resource_to_canon(
    py: Python<'_>,
    ty: wit::Resource,
    new: unsafe extern "C" fn(usize) -> u32,
    value: Py<PyAny>,
) -> u32 {
    let name = intern!(py, "__componentize_py_handle");
    if value.bind(py).hasattr(name).unwrap() {
        value.bind(py).getattr(name).unwrap().extract().unwrap()
    } else {
        let rep = value.into_ptr();
        let handle = unsafe { new(rep as usize) };
        let instance = unsafe { Py::<PyAny>::from_borrowed_ptr(py, rep) };

        instance
            .setattr(py, name, handle.into_pyobject(py).unwrap())
            .unwrap();

        let finalizer = FINALIZE
            .get()
            .unwrap()
            .call1(
                py,
                (
                    instance.clone_ref(py),
                    DROP_RESOURCE.get().unwrap(),
                    (ty.drop() as usize).into_pyobject(py).unwrap(),
                    handle,
                ),
            )
            .unwrap();

        instance
            .setattr(py, intern!(py, "finalizer"), finalizer)
            .unwrap();

        handle
    }
}

wit_dylib_ffi::export!(MyInterpreter);

// As of this writing, recent Rust `nightly` builds include a version of the
// `libc` crate that expects `wasi-libc` to define the following global
// variables, but `wasi-libc` defines them as preprocessor constants which
// aren't visible at link time, so we need to define them somewhere.  Ideally,
// we should fix this upstream, but for now we work around it:

#[unsafe(no_mangle)]
static _CLOCK_PROCESS_CPUTIME_ID: u8 = 2;
#[unsafe(no_mangle)]
static _CLOCK_THREAD_CPUTIME_ID: u8 = 3;

// Traditionally, `wit-bindgen` would provide a `cabi_realloc` implementation,
// but recent versions use a weak symbol trick to avoid conflicts when more than
// one `wit-bindgen` version is used, and that trick does not currently play
// nice with how we build this library.  So for now, we just define it ourselves
// here:
/// # Safety
/// TODO
#[unsafe(export_name = "cabi_realloc")]
pub unsafe extern "C" fn cabi_realloc(
    old_ptr: *mut u8,
    old_len: usize,
    align: usize,
    new_size: usize,
) -> *mut u8 {
    assert!(old_ptr.is_null());
    assert!(old_len == 0);

    unsafe { alloc::alloc(Layout::from_size_align(new_size, align).unwrap()) }
}

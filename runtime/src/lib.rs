#![deny(warnings)]
#![allow(
    clippy::useless_conversion,
    reason = "some pyo3 macros produce code that does this"
)]
#![allow(static_mut_refs, reason = "wit-bindgen produces code that does this")]
#![allow(unknown_lints)]
#![allow(
    unnecessary_transmutes,
    reason = "nightly warning but not supported on stable"
)]

use {
    anyhow::{Error, Result},
    componentize_py_shared::ReturnStyle,
    exports::exports::{
        self as exp, Bundled, Constructor, Function, FunctionExport, Guest, LocalResource,
        OwnedKind, OwnedType, RemoteResource, Resource, Static, Symbols,
    },
    num_bigint::BigUint,
    once_cell::sync::OnceCell,
    pyo3::{
        exceptions::PyAssertionError,
        intern,
        types::{
            PyAnyMethods, PyBool, PyBytes, PyBytesMethods, PyDict, PyList, PyListMethods,
            PyMapping, PyMappingMethods, PyModule, PyModuleMethods, PyString, PyTuple,
        },
        AsPyPointer, Borrowed, Bound, Py, PyAny, PyErr, PyObject, PyResult, Python, ToPyObject,
    },
    std::{
        alloc::{self, Layout},
        ffi::c_void,
        mem::{self, MaybeUninit},
        ops::DerefMut,
        ptr, slice, str,
        sync::{Mutex, Once},
    },
    wasi::cli::environment,
};

wit_bindgen::generate!({
    world: "init",
    path: "../wit",
    generate_all,
});

export!(MyExports);

static STUB_WASI: OnceCell<bool> = OnceCell::new();
static EXPORTS: OnceCell<Vec<Export>> = OnceCell::new();
static TYPES: OnceCell<Vec<Type>> = OnceCell::new();
static ENVIRON: OnceCell<Py<PyMapping>> = OnceCell::new();
static SOME_CONSTRUCTOR: OnceCell<PyObject> = OnceCell::new();
static OK_CONSTRUCTOR: OnceCell<PyObject> = OnceCell::new();
static ERR_CONSTRUCTOR: OnceCell<PyObject> = OnceCell::new();
static FINALIZE: OnceCell<PyObject> = OnceCell::new();
static DROP_RESOURCE: OnceCell<PyObject> = OnceCell::new();
static SEED: OnceCell<PyObject> = OnceCell::new();
static ARGV: OnceCell<Py<PyList>> = OnceCell::new();

struct Borrow {
    handle: i32,
    drop: u32,
}

static BORROWS: Mutex<Vec<Borrow>> = Mutex::new(Vec::new());

const DISCRIMINANT_FIELD_INDEX: i32 = 0;
const PAYLOAD_FIELD_INDEX: i32 = 1;

#[derive(Debug)]
struct Case {
    constructor: PyObject,
    has_payload: bool,
}

#[derive(Debug)]
enum Type {
    Record {
        constructor: PyObject,
        fields: Vec<String>,
    },
    Variant {
        types_to_discriminants: Py<PyDict>,
        cases: Vec<Case>,
    },
    Enum {
        constructor: PyObject,
        count: usize,
    },
    Flags {
        constructor: PyObject,
        u32_count: usize,
    },
    Option,
    NestingOption,
    Result,
    Tuple(usize),
    Handle,
    Resource {
        constructor: PyObject,
        local: Option<LocalResource>,
        #[allow(dead_code)]
        remote: Option<RemoteResource>,
    },
}

#[derive(Debug)]
enum Export {
    Freestanding {
        instance: PyObject,
        name: Py<PyString>,
    },
    Constructor(PyObject),
    Method(Py<PyString>),
    Static {
        class: PyObject,
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

#[link(wasm_import_module = "env")]
extern "C" {
    #[cfg_attr(target_arch = "wasm32", link_name = "componentize-py#CallIndirect")]
    fn componentize_py_call_indirect(
        context: *const c_void,
        input: *const c_void,
        output: *mut c_void,
        index: u32,
    );
}

#[pyo3::pyfunction]
#[pyo3(pass_module)]
fn call_import<'a>(
    module: Bound<'a, PyModule>,
    index: u32,
    params: Vec<Bound<'a, PyAny>>,
    result_count: usize,
) -> PyResult<Vec<&'a PyAny>> {
    let mut results = vec![MaybeUninit::<&PyAny>::uninit(); result_count];
    unsafe {
        componentize_py_call_indirect(
            &module.py() as *const _ as _,
            params.as_ptr() as _,
            results.as_mut_ptr() as _,
            index,
        );

        // todo: is this sound, or do we need to `.into_iter().map(MaybeUninit::assume_init).collect()` instead?
        Ok(mem::transmute::<Vec<MaybeUninit<&PyAny>>, Vec<&PyAny>>(
            results,
        ))
    }
}

#[pyo3::pyfunction]
#[pyo3(pass_module)]
fn drop_resource(module: &Bound<PyModule>, index: u32, handle: usize) -> PyResult<()> {
    let params = [handle];
    unsafe {
        componentize_py_call_indirect(
            &module.py() as *const _ as _,
            params.as_ptr() as _,
            ptr::null_mut(),
            index,
        );
    }
    Ok(())
}

#[pyo3::pymodule]
#[pyo3(name = "componentize_py_runtime")]
fn componentize_py_module(_py: Python<'_>, module: &Bound<PyModule>) -> PyResult<()> {
    module.add_function(pyo3::wrap_pyfunction!(call_import, module)?)?;
    module.add_function(pyo3::wrap_pyfunction!(drop_resource, module)?)
}

fn do_init(app_name: String, symbols: Symbols, stub_wasi: bool) -> Result<()> {
    pyo3::append_to_inittab!(componentize_py_module);

    pyo3::prepare_freethreaded_python();

    Python::with_gil(|py| {
        let app = match py.import_bound(app_name.as_str()) {
            Ok(app) => app,
            Err(e) => {
                e.print(py);
                return Err(e.into());
            }
        };

        STUB_WASI.set(stub_wasi).unwrap();

        EXPORTS
            .set(
                symbols
                    .exports
                    .iter()
                    .map(|export| {
                        Ok(match export {
                            FunctionExport::Bundled(Bundled {
                                module,
                                protocol,
                                name,
                            }) => Export::Freestanding {
                                name: PyString::intern_bound(py, name).into(),
                                instance: py
                                    .import_bound(module.as_str())?
                                    .getattr(protocol.as_str())?
                                    .call0()?
                                    .into(),
                            },
                            FunctionExport::Freestanding(Function { protocol, name }) => {
                                Export::Freestanding {
                                    name: PyString::intern_bound(py, name).into(),
                                    instance: app.getattr(protocol.as_str())?.call0()?.into(),
                                }
                            }
                            FunctionExport::Constructor(Constructor { module, protocol }) => {
                                Export::Constructor(
                                    py.import_bound(module.as_str())?
                                        .getattr(protocol.as_str())?
                                        .into(),
                                )
                            }
                            FunctionExport::Method(name) => {
                                Export::Method(PyString::intern_bound(py, name).into())
                            }
                            FunctionExport::Static(Static {
                                module,
                                protocol,
                                name,
                            }) => Export::Static {
                                name: PyString::intern_bound(py, name).into(),
                                class: py
                                    .import_bound(module.as_str())?
                                    .getattr(protocol.as_str())?
                                    .into(),
                            },
                        })
                    })
                    .collect::<PyResult<_>>()?,
            )
            .unwrap();

        TYPES
            .set(
                symbols
                    .types
                    .into_iter()
                    .map(|ty| {
                        Ok(match ty {
                            exp::Type::Owned(OwnedType {
                                kind,
                                package,
                                name,
                            }) => match kind {
                                OwnedKind::Record(fields) => Type::Record {
                                    constructor: py
                                        .import_bound(package.as_str())?
                                        .getattr(name.as_str())?
                                        .into(),
                                    fields,
                                },
                                OwnedKind::Variant(cases) => {
                                    let package = py.import_bound(package.as_str())?;

                                    let cases = cases
                                        .iter()
                                        .map(|case| {
                                            Ok(Case {
                                                constructor: package
                                                    .getattr(case.name.as_str())?
                                                    .into(),
                                                has_payload: case.has_payload,
                                            })
                                        })
                                        .collect::<PyResult<Vec<_>>>()?;

                                    let types_to_discriminants = PyDict::new_bound(py);
                                    for (index, case) in cases.iter().enumerate() {
                                        types_to_discriminants
                                            .set_item(&case.constructor, index)?;
                                    }

                                    Type::Variant {
                                        cases,
                                        types_to_discriminants: types_to_discriminants.into(),
                                    }
                                }
                                OwnedKind::Enum(count) => Type::Enum {
                                    constructor: py
                                        .import_bound(package.as_str())?
                                        .getattr(name.as_str())?
                                        .into(),
                                    count: count.try_into().unwrap(),
                                },
                                OwnedKind::Flags(u32_count) => Type::Flags {
                                    constructor: py
                                        .import_bound(package.as_str())?
                                        .getattr(name.as_str())?
                                        .into(),
                                    u32_count: u32_count.try_into().unwrap(),
                                },
                                OwnedKind::Resource(Resource { local, remote }) => Type::Resource {
                                    constructor: py
                                        .import_bound(package.as_str())?
                                        .getattr(name.as_str())?
                                        .into(),
                                    local,
                                    remote,
                                },
                            },
                            exp::Type::Option => Type::Option,
                            exp::Type::NestingOption => Type::NestingOption,
                            exp::Type::Result => Type::Result,
                            exp::Type::Tuple(length) => Type::Tuple(length.try_into().unwrap()),
                            exp::Type::Handle => Type::Handle,
                        })
                    })
                    .collect::<PyResult<_>>()?,
            )
            .unwrap();

        let types = py.import_bound(symbols.types_package.as_str())?;

        SOME_CONSTRUCTOR.set(types.getattr("Some")?.into()).unwrap();
        OK_CONSTRUCTOR.set(types.getattr("Ok")?.into()).unwrap();
        ERR_CONSTRUCTOR.set(types.getattr("Err")?.into()).unwrap();

        let environ = py
            .import_bound("os")?
            .getattr("environ")?
            .downcast_into::<PyMapping>()
            .unwrap();

        let keys = environ.keys()?;

        for i in 0..keys.len()? {
            environ.del_item(keys.get_item(i)?)?;
        }

        ENVIRON.set(environ.into()).unwrap();

        FINALIZE
            .set(py.import_bound("weakref")?.getattr("finalize")?.into())
            .unwrap();

        DROP_RESOURCE
            .set(
                py.import_bound("componentize_py_runtime")?
                    .getattr("drop_resource")?
                    .into(),
            )
            .unwrap();

        SEED.set(py.import_bound("random")?.getattr("seed")?.into())
            .unwrap();

        let argv = py
            .import_bound("sys")?
            .getattr("argv")?
            .downcast_into::<PyList>()
            .unwrap();

        for i in 0..argv.len() {
            argv.del_item(i)?;
        }

        ARGV.set(argv.into()).unwrap();

        Ok(())
    })
}

struct MyExports;

impl Guest for MyExports {
    fn init(app_name: String, symbols: Symbols, stub_wasi: bool) -> Result<(), String> {
        let result = do_init(app_name, symbols, stub_wasi).map_err(|e| format!("{e:?}"));

        // This tells the WASI Preview 1 component adapter to reset its state.  In particular, we want it to forget
        // about any open handles and re-request the stdio handles at runtime since we'll be running under a brand
        // new host.
        #[link(wasm_import_module = "wasi_snapshot_preview1")]
        extern "C" {
            #[cfg_attr(target_arch = "wasm32", link_name = "reset_adapter_state")]
            fn reset_adapter_state();
        }

        // This tells wasi-libc to reset its preopen state, forcing re-initialization at runtime.
        #[link(wasm_import_module = "env")]
        extern "C" {
            #[cfg_attr(target_arch = "wasm32", link_name = "__wasilibc_reset_preopens")]
            fn wasilibc_reset_preopens();
        }

        unsafe {
            reset_adapter_state();
            wasilibc_reset_preopens();
        }

        result
    }
}

/// # Safety
/// TODO
#[export_name = "componentize-py#Dispatch"]
pub unsafe extern "C" fn componentize_py_dispatch(
    export: usize,
    from_canon: u32,
    to_canon: u32,
    param_count: u32,
    return_style: ReturnStyle,
    params_canon: *const c_void,
    results_canon: *mut c_void,
) {
    Python::with_gil(|py| {
        let mut params_py = vec![MaybeUninit::<&PyAny>::uninit(); param_count.try_into().unwrap()];

        componentize_py_call_indirect(
            &py as *const _ as _,
            params_canon,
            params_py.as_mut_ptr() as _,
            from_canon,
        );

        // todo: is this sound, or do we need to `.into_iter().map(MaybeUninit::assume_init).collect()` instead?
        let mut params_py = mem::transmute::<Vec<MaybeUninit<&PyAny>>, Vec<&PyAny>>(params_py)
            .into_iter()
            .map(|p| Bound::from_borrowed_ptr(py, p.as_ptr()));

        if !*STUB_WASI.get().unwrap() {
            static ONCE: Once = Once::new();
            ONCE.call_once(|| {
                // We must call directly into the host to get the runtime environment since libc's version will only
                // contain the build-time pre-init snapshot.
                let environ = ENVIRON.get().unwrap().bind(py);
                for (k, v) in environment::get_environment() {
                    environ.set_item(k, v).unwrap();
                }

                // Likewise for CLI arguments.
                for arg in environment::get_arguments() {
                    ARGV.get().unwrap().bind(py).append(arg).unwrap();
                }

                // Call `random.seed()` to ensure we get a fresh seed rather than the one that got baked in during
                // pre-init.
                SEED.get().unwrap().call0(py).unwrap();
            });
        }

        let export = &EXPORTS.get().unwrap()[export];
        let result = match export {
            Export::Freestanding { instance, name } => {
                instance.call_method1(py, name, PyTuple::new_bound(py, params_py))
            }
            Export::Constructor(class) => class.call1(py, PyTuple::new_bound(py, params_py)),
            Export::Method(name) => params_py
                // Call method on self with remaining iterator elements
                .next()
                .unwrap()
                .call_method1(name, PyTuple::new_bound(py, params_py))
                .map(|r| r.into()),
            Export::Static { class, name } => class
                .getattr(py, name)
                .and_then(|function| function.call1(py, PyTuple::new_bound(py, params_py))),
        };

        let result = match return_style {
            ReturnStyle::Normal => match result {
                Ok(result) => result,
                Err(error) => {
                    error.print(py);
                    panic!("Python function threw an unexpected exception")
                }
            },
            ReturnStyle::Result => match result {
                Ok(result) => OK_CONSTRUCTOR.get().unwrap().call1(py, (result,)).unwrap(),
                Err(result) => {
                    if ERR_CONSTRUCTOR
                        .get()
                        .unwrap()
                        .bind(py)
                        .eq(result.get_type_bound(py))
                        .unwrap()
                    {
                        result.to_object(py)
                    } else {
                        result.print(py);
                        panic!("Python function threw an unexpected exception")
                    }
                }
            },
        };

        let result_array = [result];

        componentize_py_call_indirect(
            &py as *const _ as _,
            result_array.as_ptr() as *const _ as _,
            results_canon,
            to_canon,
        );

        let borrows = mem::take(BORROWS.lock().unwrap().deref_mut());
        for Borrow { handle, drop } in borrows {
            let params = [handle];
            unsafe {
                componentize_py_call_indirect(
                    &py as *const _ as _,
                    params.as_ptr() as _,
                    ptr::null_mut(),
                    drop,
                );
            }
        }
    });
}

/// # Safety
/// TODO
#[export_name = "componentize-py#Allocate"]
pub unsafe extern "C" fn componentize_py_allocate(size: usize, align: usize) -> *mut u8 {
    alloc::alloc(Layout::from_size_align(size, align).unwrap())
}

/// # Safety
/// TODO
#[export_name = "componentize-py#Free"]
pub unsafe extern "C" fn componentize_py_free(ptr: *mut u8, size: usize, align: usize) {
    alloc::dealloc(ptr, Layout::from_size_align(size, align).unwrap())
}

#[export_name = "componentize-py#ToCanonBool"]
pub extern "C" fn componentize_py_to_canon_bool(_py: &Python, value: Borrowed<PyAny>) -> u32 {
    if value.is_truthy().unwrap() {
        1
    } else {
        0
    }
}

#[export_name = "componentize-py#ToCanonI32"]
pub extern "C" fn componentize_py_to_canon_i32(_py: &Python, value: Borrowed<PyAny>) -> i32 {
    value.extract().unwrap()
}

#[export_name = "componentize-py#ToCanonU32"]
pub extern "C" fn componentize_py_to_canon_u32(_py: &Python, value: Borrowed<PyAny>) -> u32 {
    value.extract().unwrap()
}

#[export_name = "componentize-py#ToCanonI64"]
pub extern "C" fn componentize_py_to_canon_i64(_py: &Python, value: Borrowed<PyAny>) -> i64 {
    value.extract().unwrap()
}

#[export_name = "componentize-py#ToCanonU64"]
pub extern "C" fn componentize_py_to_canon_u64(_py: &Python, value: Borrowed<PyAny>) -> u64 {
    value.extract().unwrap()
}

#[export_name = "componentize-py#ToCanonF32"]
pub extern "C" fn componentize_py_to_canon_f32(_py: &Python, value: Borrowed<PyAny>) -> f32 {
    value.extract().unwrap()
}

#[export_name = "componentize-py#ToCanonF64"]
pub extern "C" fn componentize_py_to_canon_f64(_py: &Python, value: Borrowed<PyAny>) -> f64 {
    value.extract().unwrap()
}

#[export_name = "componentize-py#ToCanonChar"]
pub extern "C" fn componentize_py_to_canon_char(_py: &Python, value: Borrowed<PyAny>) -> u32 {
    let value = value.extract::<String>().unwrap();
    assert!(value.chars().count() == 1);
    value.chars().next().unwrap() as u32
}

/// # Safety
/// TODO
#[export_name = "componentize-py#ToCanonString"]
pub unsafe extern "C" fn componentize_py_to_canon_string(
    _py: &Python,
    value: Borrowed<PyAny>,
    destination: *mut (*const u8, usize),
) {
    let value = value.extract::<String>().unwrap().into_bytes();
    unsafe {
        let result = alloc::alloc(Layout::from_size_align(value.len(), 1).unwrap());
        ptr::copy_nonoverlapping(value.as_ptr(), result, value.len());
        destination.write((result, value.len()));
    }
}

#[export_name = "componentize-py#GetField"]
pub extern "C" fn componentize_py_get_field<'a>(
    py: &'a Python,
    value: Borrowed<'_, 'a, PyAny>,
    ty: usize,
    field: usize,
) -> Bound<'a, PyAny> {
    match &TYPES.get().unwrap()[ty] {
        Type::Record { fields, .. } => value.getattr(fields[field].as_str()).unwrap(),
        Type::Variant {
            types_to_discriminants,
            cases,
        } => {
            let discriminant = types_to_discriminants
                .bind(*py)
                .get_item(value.get_type())
                .unwrap();

            match i32::try_from(field).unwrap() {
                DISCRIMINANT_FIELD_INDEX => discriminant,
                PAYLOAD_FIELD_INDEX => {
                    if cases[discriminant.extract::<usize>().unwrap()].has_payload {
                        value.getattr("value").unwrap()
                    } else {
                        py.None().into_bound(*py)
                    }
                }
                _ => unreachable!(),
            }
        }
        Type::Enum { .. } => match i32::try_from(field).unwrap() {
            DISCRIMINANT_FIELD_INDEX => value.getattr("value").unwrap(),
            PAYLOAD_FIELD_INDEX => py.None().into_bound(*py),
            _ => unreachable!(),
        },
        Type::Flags { u32_count, .. } => {
            assert!(field < *u32_count);
            let value = value
                .getattr("value")
                .unwrap()
                .extract::<BigUint>()
                .unwrap()
                .iter_u32_digits()
                .nth(field)
                .unwrap_or(0);

            u32::cast_signed(value).to_object(*py).into_bound(*py)
        }
        Type::Option => match i32::try_from(field).unwrap() {
            DISCRIMINANT_FIELD_INDEX => if value.is_none() { 0 } else { 1 }
                .to_object(*py)
                .into_bound(*py),
            PAYLOAD_FIELD_INDEX => value.to_owned(),
            _ => unreachable!(),
        },
        Type::NestingOption => match i32::try_from(field).unwrap() {
            DISCRIMINANT_FIELD_INDEX => if value.is_none() { 0 } else { 1 }
                .to_object(*py)
                .into_bound(*py),
            PAYLOAD_FIELD_INDEX => {
                if value.is_none() {
                    value.to_owned()
                } else {
                    value.getattr("value").unwrap()
                }
            }
            _ => unreachable!(),
        },
        Type::Result => match i32::try_from(field).unwrap() {
            DISCRIMINANT_FIELD_INDEX => if OK_CONSTRUCTOR
                .get()
                .unwrap()
                .bind(*py)
                .eq(value.get_type())
                .unwrap()
            {
                0_i32
            } else if ERR_CONSTRUCTOR
                .get()
                .unwrap()
                .bind(*py)
                .eq(value.get_type())
                .unwrap()
            {
                1
            } else {
                unreachable!()
            }
            .to_object(*py)
            .into_bound(*py),
            PAYLOAD_FIELD_INDEX => value.getattr("value").unwrap(),
            _ => unreachable!(),
        },
        Type::Tuple(length) => {
            assert!(field < *length);
            value
                .downcast::<PyTuple>()
                .unwrap()
                .get_item(field)
                .unwrap()
        }
        Type::Handle | Type::Resource { .. } => unreachable!(),
    }
}

#[export_name = "componentize-py#GetListLength"]
pub extern "C" fn componentize_py_get_list_length(_py: &Python, value: Borrowed<PyAny>) -> usize {
    if let Ok(bytes) = value.downcast::<PyBytes>() {
        bytes.len().unwrap()
    } else {
        value.downcast::<PyList>().unwrap().len()
    }
}

#[export_name = "componentize-py#GetListElement"]
pub extern "C" fn componentize_py_get_list_element<'a>(
    _py: &Python<'a>,
    value: Borrowed<'_, 'a, PyAny>,
    index: usize,
) -> Bound<'a, PyAny> {
    value.downcast::<PyList>().unwrap().get_item(index).unwrap()
}

#[export_name = "componentize-py#FromCanonBool"]
pub extern "C" fn componentize_py_from_canon_bool<'a>(
    py: &Python<'a>,
    value: u32,
) -> Bound<'a, PyBool> {
    PyBool::new_bound(*py, value != 0).to_owned()
}

#[export_name = "componentize-py#FromCanonI32"]
pub extern "C" fn componentize_py_from_canon_i32(py: &Python, value: i32) -> Py<PyAny> {
    value.to_object(*py)
}

#[export_name = "componentize-py#FromCanonU32"]
pub extern "C" fn componentize_py_from_canon_u32(py: &Python, value: u32) -> Py<PyAny> {
    value.to_object(*py)
}

#[export_name = "componentize-py#FromCanonI64"]
pub extern "C" fn componentize_py_from_canon_i64(py: &Python, value: i64) -> Py<PyAny> {
    value.to_object(*py)
}

#[export_name = "componentize-py#FromCanonU64"]
pub extern "C" fn componentize_py_from_canon_u64(py: &Python, value: u64) -> Py<PyAny> {
    value.to_object(*py)
}

#[export_name = "componentize-py#FromCanonF32"]
pub extern "C" fn componentize_py_from_canon_f32(py: &Python, value: f32) -> Py<PyAny> {
    value.to_object(*py)
}

#[export_name = "componentize-py#FromCanonF64"]
pub extern "C" fn componentize_py_from_canon_f64(py: &Python, value: f64) -> Py<PyAny> {
    value.to_object(*py)
}

#[export_name = "componentize-py#FromCanonChar"]
pub extern "C" fn componentize_py_from_canon_char(py: &Python, value: u32) -> Py<PyAny> {
    char::from_u32(value).unwrap().to_string().to_object(*py)
}

/// # Safety
/// TODO
#[export_name = "componentize-py#FromCanonString"]
pub unsafe extern "C" fn componentize_py_from_canon_string<'a>(
    py: &Python<'a>,
    data: *const u8,
    len: usize,
) -> Bound<'a, PyString> {
    PyString::new_bound(*py, unsafe {
        str::from_utf8_unchecked(slice::from_raw_parts(data, len))
    })
}

/// # Safety
/// TODO
#[export_name = "componentize-py#Init"]
pub unsafe extern "C" fn componentize_py_init<'a>(
    py: &Python<'a>,
    ty: usize,
    data: *const &'a PyAny,
    len: usize,
) -> Bound<'a, PyAny> {
    match &TYPES.get().unwrap()[ty] {
        Type::Record { constructor, .. } => {
            let elements = slice::from_raw_parts(data, len)
                .iter()
                .map(|e| Bound::from_borrowed_ptr(*py, e.as_ptr()));
            constructor
                .call1(*py, PyTuple::new_bound(*py, elements))
                .unwrap()
                .into_bound(*py)
        }
        Type::Variant { cases, .. } => {
            assert!(len == 2);
            let discriminant = Bound::from_borrowed_ptr(
                *py,
                ptr::read(data.offset(isize::try_from(DISCRIMINANT_FIELD_INDEX).unwrap())).as_ptr(),
            )
            .extract::<u32>()
            .unwrap();
            let case = &cases[usize::try_from(discriminant).unwrap()];
            if case.has_payload {
                case.constructor.call1(
                    *py,
                    (ptr::read(
                        data.offset(isize::try_from(PAYLOAD_FIELD_INDEX).unwrap()),
                    ),),
                )
            } else {
                case.constructor.call1(*py, ())
            }
            .unwrap()
            .into_bound(*py)
        }
        Type::Enum { constructor, count } => {
            assert!(len == 2);
            let discriminant = Bound::from_borrowed_ptr(
                *py,
                ptr::read(data.offset(isize::try_from(DISCRIMINANT_FIELD_INDEX).unwrap())).as_ptr(),
            )
            .extract::<usize>()
            .unwrap();
            assert!(discriminant < *count);
            constructor
                .call1(
                    *py,
                    (ptr::read(data.offset(
                        isize::try_from(DISCRIMINANT_FIELD_INDEX).unwrap(),
                    )),),
                )
                .unwrap()
                .into_bound(*py)
        }
        Type::Flags {
            constructor,
            u32_count,
        } => {
            assert!(len == *u32_count);
            constructor
                .call1(
                    *py,
                    (BigUint::new(
                        slice::from_raw_parts(data, len)
                            .iter()
                            .map(|v| {
                                i32::cast_unsigned(
                                    Bound::from_borrowed_ptr(*py, v.as_ptr()).extract().unwrap(),
                                )
                            })
                            .collect(),
                    ),),
                )
                .unwrap()
                .into_bound(*py)
        }
        Type::Option => {
            assert!(len == 2);
            let discriminant = Bound::from_borrowed_ptr(
                *py,
                ptr::read(data.offset(isize::try_from(DISCRIMINANT_FIELD_INDEX).unwrap())).as_ptr(),
            )
            .extract::<u32>()
            .unwrap();
            match discriminant {
                0 => py.None().into_bound(*py),
                1 => Bound::from_borrowed_ptr(
                    *py,
                    ptr::read(data.offset(isize::try_from(PAYLOAD_FIELD_INDEX).unwrap())).as_ptr(),
                ),
                _ => unreachable!(),
            }
        }
        Type::NestingOption => {
            assert!(len == 2);
            let discriminant = Bound::from_borrowed_ptr(
                *py,
                ptr::read(data.offset(isize::try_from(DISCRIMINANT_FIELD_INDEX).unwrap())).as_ptr(),
            )
            .extract::<u32>()
            .unwrap();

            match discriminant {
                0 => py.None().into_bound(*py),

                1 => SOME_CONSTRUCTOR
                    .get()
                    .unwrap()
                    .call1(
                        *py,
                        (ptr::read(
                            data.offset(isize::try_from(PAYLOAD_FIELD_INDEX).unwrap()),
                        ),),
                    )
                    .unwrap()
                    .into_bound(*py),

                _ => unreachable!(),
            }
        }
        Type::Result => {
            assert!(len == 2);
            let discriminant = Bound::from_borrowed_ptr(
                *py,
                ptr::read(data.offset(isize::try_from(DISCRIMINANT_FIELD_INDEX).unwrap())).as_ptr(),
            )
            .extract::<u32>()
            .unwrap();

            match discriminant {
                0 => OK_CONSTRUCTOR.get().unwrap(),
                1 => ERR_CONSTRUCTOR.get().unwrap(),
                _ => unreachable!(),
            }
            .call1(
                *py,
                (ptr::read(
                    data.offset(isize::try_from(PAYLOAD_FIELD_INDEX).unwrap()),
                ),),
            )
            .unwrap()
            .into_bound(*py)
        }
        Type::Tuple(length) => {
            assert!(*length == len);
            let elements = slice::from_raw_parts(data, len)
                .iter()
                .map(|e| Bound::from_borrowed_ptr(*py, e.as_ptr()));
            PyTuple::new_bound(*py, elements).into_any()
        }
        Type::Handle | Type::Resource { .. } => unreachable!(),
    }
}

#[export_name = "componentize-py#MakeList"]
pub extern "C" fn componentize_py_make_list<'a>(py: &Python<'a>) -> Bound<'a, PyList> {
    PyList::empty_bound(*py)
}

#[export_name = "componentize-py#ListAppend"]
pub extern "C" fn componentize_py_list_append(
    _py: &Python,
    list: Borrowed<PyList>,
    element: Borrowed<PyAny>,
) {
    list.append(element).unwrap();
}

#[export_name = "componentize-py#None"]
pub extern "C" fn componentize_py_none(py: &Python) -> Py<PyAny> {
    py.None()
}

/// # Safety
/// TODO
#[export_name = "componentize-py#GetBytes"]
pub unsafe extern "C" fn componentize_py_get_bytes(
    _py: &Python,
    src: Borrowed<PyBytes>,
    dst: *mut u8,
    len: usize,
) {
    assert_eq!(len, src.len().unwrap());
    slice::from_raw_parts_mut(dst, len).copy_from_slice(src.as_bytes())
}

/// # Safety
/// TODO
#[export_name = "componentize-py#MakeBytes"]
pub unsafe extern "C" fn componentize_py_make_bytes<'a>(
    py: &Python<'a>,
    src: *const u8,
    len: usize,
) -> Bound<'a, PyBytes> {
    PyBytes::new_bound_with(*py, len, |dst| {
        dst.copy_from_slice(slice::from_raw_parts(src, len));
        Ok(())
    })
    .unwrap()
}

#[export_name = "componentize-py#FromCanonHandle"]
pub extern "C" fn componentize_py_from_canon_handle<'a>(
    py: &Python<'a>,
    value: i32,
    borrow: i32,
    local: i32,
    resource: i32,
) -> Bound<'a, PyAny> {
    let ty = &TYPES.get().unwrap()[usize::try_from(resource).unwrap()];
    let Type::Resource {
        constructor,
        local: resource_local,
        remote: resource_remote,
    } = ty
    else {
        panic!("expected resource, found {ty:?}");
    };

    if local != 0 {
        if borrow != 0 {
            unsafe { PyObject::from_borrowed_ptr(*py, value as usize as _) }.into_bound(*py)
        } else {
            let Some(LocalResource { rep, .. }) = resource_local else {
                panic!("expected local resource, found {ty:?}");
            };

            let rep = {
                let params = [value];
                let mut results = [MaybeUninit::<usize>::uninit()];
                unsafe {
                    componentize_py_call_indirect(
                        py as *const _ as _,
                        params.as_ptr() as _,
                        results.as_mut_ptr() as _,
                        *rep,
                    );
                    results[0].assume_init()
                }
            };

            let value = unsafe { PyObject::from_borrowed_ptr(*py, rep as _) }.into_bound(*py);

            value
                .delattr(intern!(*py, "__componentize_py_handle"))
                .unwrap();

            value
                .getattr(intern!(*py, "finalizer"))
                .unwrap()
                .call_method0(intern!(*py, "detach"))
                .unwrap();

            value
        }
    } else {
        let Some(RemoteResource { drop }) = resource_remote else {
            panic!("expected remote resource, found {ty:?}");
        };

        if borrow != 0 {
            BORROWS.lock().unwrap().push(Borrow {
                handle: value,
                drop: *drop,
            });
        }

        let instance = constructor
            .call_method1(
                *py,
                intern!(*py, "__new__"),
                PyTuple::new_bound(*py, [constructor]),
            )
            .unwrap();

        let handle = value.to_object(*py);

        instance
            .setattr(*py, intern!(*py, "handle"), handle.clone_ref(*py))
            .unwrap();

        let finalizer = FINALIZE
            .get()
            .unwrap()
            .call1(
                *py,
                (
                    instance.clone_ref(*py),
                    DROP_RESOURCE.get().unwrap(),
                    drop.to_object(*py),
                    handle,
                ),
            )
            .unwrap();

        instance
            .setattr(*py, intern!(*py, "finalizer"), finalizer)
            .unwrap();

        instance.into_bound(*py)
    }
}

#[export_name = "componentize-py#ToCanonHandle"]
pub extern "C" fn componentize_py_to_canon_handle(
    py: &Python,
    value: Borrowed<PyAny>,
    borrow: i32,
    local: i32,
    resource: i32,
) -> u32 {
    if local != 0 {
        let ty = &TYPES.get().unwrap()[usize::try_from(resource).unwrap()];
        let Type::Resource {
            local: Some(LocalResource { new, drop, .. }),
            ..
        } = ty
        else {
            panic!("expected local resource, found {ty:?}");
        };

        let name = intern!(*py, "__componentize_py_handle");
        if value.hasattr(name).unwrap() {
            value.getattr(name).unwrap().extract().unwrap()
        } else {
            let rep = PyObject::from(value.to_owned()).into_ptr();
            let handle = {
                let params = [rep as usize];
                let mut results = [MaybeUninit::<u32>::uninit()];
                unsafe {
                    componentize_py_call_indirect(
                        py as *const _ as _,
                        params.as_ptr() as _,
                        results.as_mut_ptr() as _,
                        *new,
                    );
                    results[0].assume_init()
                }
            };

            let instance = unsafe { PyObject::from_borrowed_ptr(*py, rep) };

            instance.setattr(*py, name, handle.to_object(*py)).unwrap();

            let finalizer = FINALIZE
                .get()
                .unwrap()
                .call1(
                    *py,
                    (
                        instance.clone_ref(*py),
                        DROP_RESOURCE.get().unwrap(),
                        drop.to_object(*py),
                        handle,
                    ),
                )
                .unwrap();

            instance
                .setattr(*py, intern!(*py, "finalizer"), finalizer)
                .unwrap();

            handle
        }
    } else {
        if borrow == 0 {
            value
                .getattr(intern!(*py, "finalizer"))
                .unwrap()
                .call_method0(intern!(*py, "detach"))
                .unwrap();
        }

        value
            .getattr(intern!(*py, "handle"))
            .unwrap()
            .extract()
            .unwrap()
    }
}

// As of this writing, recent Rust `nightly` builds include a version of the `libc` crate that expects `wasi-libc`
// to define the following global variables, but `wasi-libc` defines them as preprocessor constants which aren't
// visible at link time, so we need to define them somewhere.  Ideally, we should fix this upstream, but for now we
// work around it:

#[no_mangle]
static _CLOCK_PROCESS_CPUTIME_ID: u8 = 2;
#[no_mangle]
static _CLOCK_THREAD_CPUTIME_ID: u8 = 3;

// Traditionally, `wit-bindgen` would provide a `cabi_realloc` implementation, but recent versions use a weak
// symbol trick to avoid conflicts when more than one `wit-bindgen` version is used, and that trick does not
// currently play nice with how we build this library.  So for now, we just define it ourselves here:
/// # Safety
/// TODO
#[export_name = "cabi_realloc"]
pub unsafe extern "C" fn cabi_realloc(
    old_ptr: *mut u8,
    old_len: usize,
    align: usize,
    new_size: usize,
) -> *mut u8 {
    assert!(old_ptr.is_null());
    assert!(old_len == 0);

    alloc::alloc(Layout::from_size_align(new_size, align).unwrap())
}

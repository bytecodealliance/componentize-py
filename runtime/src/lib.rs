#![deny(warnings)]

use {
    anyhow::{Error, Result},
    componentize_py_shared::ReturnStyle,
    exports::exports::{self as exp, Exports, OwnedKind, OwnedType, RawUnionType, Symbols},
    num_bigint::BigUint,
    once_cell::sync::OnceCell,
    pyo3::{
        exceptions::PyAssertionError,
        types::{PyBytes, PyDict, PyFloat, PyInt, PyList, PyMapping, PyModule, PyString, PyTuple},
        Py, PyAny, PyErr, PyObject, PyResult, Python, ToPyObject,
    },
    std::{
        alloc::{self, Layout},
        env,
        ffi::c_void,
        mem::{self, MaybeUninit},
        ptr, slice, str,
    },
};

wit_bindgen::generate!({
    world: "init",
    path: "../wit/init.wit"
});

static EXPORTS: OnceCell<Vec<(Py<PyString>, PyObject)>> = OnceCell::new();
static TYPES: OnceCell<Vec<Type>> = OnceCell::new();
static ENVIRON: OnceCell<Py<PyMapping>> = OnceCell::new();
static SOME_CONSTRUCTOR: OnceCell<PyObject> = OnceCell::new();
static OK_CONSTRUCTOR: OnceCell<PyObject> = OnceCell::new();
static ERR_CONSTRUCTOR: OnceCell<PyObject> = OnceCell::new();

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
    RawUnion {
        types_to_discriminants: Py<PyDict>,
        other_discriminant: Option<usize>,
    },
    Flags {
        constructor: PyObject,
        u32_count: usize,
    },
    Option,
    NestingOption,
    Result,
    Tuple(usize),
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
    module: &'a PyModule,
    index: u32,
    params: Vec<&PyAny>,
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

#[pyo3::pymodule]
#[pyo3(name = "componentize_py_runtime")]
fn componentize_py_module(_py: Python<'_>, module: &PyModule) -> PyResult<()> {
    module.add_function(pyo3::wrap_pyfunction!(call_import, module)?)
}

fn do_init(app_name: String, symbols: Symbols) -> Result<()> {
    pyo3::append_to_inittab!(componentize_py_module);

    pyo3::prepare_freethreaded_python();

    Python::with_gil(|py| {
        let app = py.import(app_name.as_str())?;

        EXPORTS
            .set(
                symbols
                    .exports
                    .iter()
                    .map(|function| {
                        Ok((
                            PyString::intern(py, &function.name).into(),
                            app.getattr(function.protocol.as_str())?.call0()?.into(),
                        ))
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
                                        .import(package.as_str())?
                                        .getattr(name.as_str())?
                                        .into(),
                                    fields,
                                },
                                OwnedKind::Variant(cases) => {
                                    let package = py.import(package.as_str())?;

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

                                    let types_to_discriminants = PyDict::new(py);
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
                                        .import(package.as_str())?
                                        .getattr(name.as_str())?
                                        .into(),
                                    count: count.try_into().unwrap(),
                                },
                                OwnedKind::RawUnion(types) => {
                                    let types_to_discriminants = PyDict::new(py);
                                    let mut other_discriminant = None;
                                    for (index, ty) in types.iter().enumerate() {
                                        let ty = match ty {
                                            RawUnionType::Int => Some(py.get_type::<PyInt>()),
                                            RawUnionType::Float => Some(py.get_type::<PyFloat>()),
                                            RawUnionType::Str => Some(py.get_type::<PyString>()),
                                            RawUnionType::Other => None,
                                        };

                                        if let Some(ty) = ty {
                                            types_to_discriminants.set_item(ty, index)?;
                                        } else {
                                            assert!(other_discriminant.is_none());
                                            other_discriminant = Some(index);
                                        }
                                    }

                                    Type::RawUnion {
                                        types_to_discriminants: types_to_discriminants.into(),
                                        other_discriminant,
                                    }
                                }
                                OwnedKind::Flags(u32_count) => Type::Flags {
                                    constructor: py
                                        .import(package.as_str())?
                                        .getattr(name.as_str())?
                                        .into(),
                                    u32_count: u32_count.try_into().unwrap(),
                                },
                            },
                            exp::Type::Option => Type::Option,
                            exp::Type::NestingOption => Type::NestingOption,
                            exp::Type::Result => Type::Result,
                            exp::Type::Tuple(length) => Type::Tuple(length.try_into().unwrap()),
                        })
                    })
                    .collect::<PyResult<_>>()?,
            )
            .unwrap();

        let types = py.import(symbols.types_package.as_str())?;

        SOME_CONSTRUCTOR.set(types.getattr("Some")?.into()).unwrap();
        OK_CONSTRUCTOR.set(types.getattr("Ok")?.into()).unwrap();
        ERR_CONSTRUCTOR.set(types.getattr("Err")?.into()).unwrap();

        let environ = py
            .import("os")?
            .getattr("environ")?
            .downcast::<PyMapping>()
            .unwrap();

        let keys = environ.keys()?;

        for i in 0..keys.len()? {
            environ.del_item(keys.get_item(i)?)?;
        }

        ENVIRON.set(environ.into()).unwrap();

        Ok(())
    })
}

struct MyExports;

impl Exports for MyExports {
    fn init(app_name: String, symbols: Symbols) -> Result<(), String> {
        do_init(app_name, symbols).map_err(|e| format!("{e:?}"))
    }
}

export_init!(MyExports);

/// # Safety
/// TODO
#[export_name = "componentize-py#Dispatch"]
pub unsafe extern "C" fn componentize_py_dispatch(
    export: usize,
    lift: u32,
    lower: u32,
    param_count: u32,
    return_style: ReturnStyle,
    params: *const c_void,
    results: *mut c_void,
) {
    run_ctors();

    Python::with_gil(|py| {
        let mut params_lifted =
            vec![MaybeUninit::<&PyAny>::uninit(); param_count.try_into().unwrap()];

        componentize_py_call_indirect(
            &py as *const _ as _,
            params,
            params_lifted.as_mut_ptr() as _,
            lift,
        );

        // todo: is this sound, or do we need to `.into_iter().map(MaybeUninit::assume_init).collect()` instead?
        let params_lifted = mem::transmute::<Vec<MaybeUninit<&PyAny>>, Vec<&PyAny>>(params_lifted);

        let environ = ENVIRON.get().unwrap().as_ref(py);
        for (k, v) in env::vars() {
            environ.set_item(k, v).unwrap();
        }

        let (name, target) = &EXPORTS.get().unwrap()[export];
        let result = target.call_method1(py, name.as_ref(py), PyTuple::new(py, params_lifted));

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
                Err(result) => result.to_object(py),
            },
        };

        let result = result.into_ref(py);
        let result_array = [result];

        componentize_py_call_indirect(
            &py as *const _ as _,
            result_array.as_ptr() as *const _ as _,
            results,
            lower,
        );
    });
}

pub fn run_ctors() {
    unsafe {
        extern "C" {
            fn __wasm_call_ctors();
        }
        __wasm_call_ctors();
    }
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

#[export_name = "componentize-py#LowerI32"]
pub extern "C" fn componentize_py_lower_i32(_py: &Python, value: &PyAny) -> i32 {
    value.extract().unwrap()
}

#[export_name = "componentize-py#LowerI64"]
pub extern "C" fn componentize_py_lower_i64(_py: &Python, value: &PyAny) -> i64 {
    value.extract().unwrap()
}

#[export_name = "componentize-py#LowerF32"]
pub extern "C" fn componentize_py_lower_f32(_py: &Python, value: &PyAny) -> f32 {
    value.extract().unwrap()
}

#[export_name = "componentize-py#LowerF64"]
pub extern "C" fn componentize_py_lower_f64(_py: &Python, value: &PyAny) -> f64 {
    value.extract().unwrap()
}

#[export_name = "componentize-py#LowerChar"]
pub extern "C" fn componentize_py_lower_char(_py: &Python, value: &PyAny) -> u32 {
    let value = value.extract::<String>().unwrap();
    assert!(value.chars().count() == 1);
    value.chars().next().unwrap() as u32
}

/// # Safety
/// TODO
#[export_name = "componentize-py#LowerString"]
pub unsafe extern "C" fn componentize_py_lower_string(
    _py: &Python,
    value: &PyAny,
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
    value: &'a PyAny,
    ty: usize,
    field: usize,
) -> &'a PyAny {
    match &TYPES.get().unwrap()[ty] {
        Type::Record { fields, .. } => value.getattr(fields[field].as_str()).unwrap(),
        Type::Variant {
            types_to_discriminants,
            cases,
        } => {
            let discriminant = types_to_discriminants
                .as_ref(*py)
                .get_item(value.get_type())
                .unwrap();

            match i32::try_from(field).unwrap() {
                DISCRIMINANT_FIELD_INDEX => discriminant,
                PAYLOAD_FIELD_INDEX => {
                    if cases[discriminant.extract::<usize>().unwrap()].has_payload {
                        value.getattr("value").unwrap()
                    } else {
                        py.None().into_ref(*py)
                    }
                }
                _ => unreachable!(),
            }
        }
        Type::Enum { .. } => match i32::try_from(field).unwrap() {
            DISCRIMINANT_FIELD_INDEX => value.getattr("value").unwrap(),
            PAYLOAD_FIELD_INDEX => py.None().into_ref(*py),
            _ => unreachable!(),
        },
        Type::RawUnion {
            types_to_discriminants,
            other_discriminant,
        } => match i32::try_from(field).unwrap() {
            DISCRIMINANT_FIELD_INDEX => types_to_discriminants
                .as_ref(*py)
                .get_item(value.get_type())
                .or_else(|| other_discriminant.map(|v| v.to_object(*py).into_ref(*py)))
                .unwrap(),
            PAYLOAD_FIELD_INDEX => value,
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

            unsafe { mem::transmute::<u32, i32>(value) }
                .to_object(*py)
                .into_ref(*py)
                .downcast()
                .unwrap()
        }
        Type::Option => match i32::try_from(field).unwrap() {
            DISCRIMINANT_FIELD_INDEX => if value.is_none() { 0 } else { 1 }
                .to_object(*py)
                .into_ref(*py)
                .downcast()
                .unwrap(),
            PAYLOAD_FIELD_INDEX => value,
            _ => unreachable!(),
        },
        Type::NestingOption => match i32::try_from(field).unwrap() {
            DISCRIMINANT_FIELD_INDEX => if value.is_none() { 0 } else { 1 }
                .to_object(*py)
                .into_ref(*py)
                .downcast()
                .unwrap(),
            PAYLOAD_FIELD_INDEX => {
                if value.is_none() {
                    value
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
                .as_ref(*py)
                .eq(value.get_type())
                .unwrap()
            {
                0_i32
            } else if ERR_CONSTRUCTOR
                .get()
                .unwrap()
                .as_ref(*py)
                .eq(value.get_type())
                .unwrap()
            {
                1
            } else {
                unreachable!()
            }
            .to_object(*py)
            .into_ref(*py),
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
    }
}

#[export_name = "componentize-py#GetListLength"]
pub extern "C" fn componentize_py_get_list_length(_py: &Python, value: &PyAny) -> usize {
    if let Ok(bytes) = value.downcast::<PyBytes>() {
        bytes.len().unwrap()
    } else {
        value.downcast::<PyList>().unwrap().len()
    }
}

#[export_name = "componentize-py#GetListElement"]
pub extern "C" fn componentize_py_get_list_element<'a>(
    _py: &'a Python,
    value: &'a PyAny,
    index: usize,
) -> &'a PyAny {
    value.downcast::<PyList>().unwrap().get_item(index).unwrap()
}

#[export_name = "componentize-py#LiftI32"]
pub extern "C" fn componentize_py_lift_i32<'a>(py: &'a Python<'a>, value: i32) -> &'a PyAny {
    value.to_object(*py).into_ref(*py).downcast().unwrap()
}

#[export_name = "componentize-py#LiftI64"]
pub extern "C" fn componentize_py_lift_i64<'a>(py: &'a Python<'a>, value: i64) -> &'a PyAny {
    value.to_object(*py).into_ref(*py).downcast().unwrap()
}

#[export_name = "componentize-py#LiftF32"]
pub extern "C" fn componentize_py_lift_f32<'a>(py: &'a Python<'a>, value: f32) -> &'a PyAny {
    value.to_object(*py).into_ref(*py).downcast().unwrap()
}

#[export_name = "componentize-py#LiftF64"]
pub extern "C" fn componentize_py_lift_f64<'a>(py: &'a Python<'a>, value: f64) -> &'a PyAny {
    value.to_object(*py).into_ref(*py).downcast().unwrap()
}

#[export_name = "componentize-py#LiftChar"]
pub extern "C" fn componentize_py_lift_char<'a>(py: &'a Python<'a>, value: u32) -> &'a PyAny {
    char::from_u32(value)
        .unwrap()
        .to_string()
        .to_object(*py)
        .into_ref(*py)
        .downcast()
        .unwrap()
}

/// # Safety
/// TODO
#[export_name = "componentize-py#LiftString"]
pub unsafe extern "C" fn componentize_py_lift_string<'a>(
    py: &'a Python,
    data: *const u8,
    len: usize,
) -> &'a PyAny {
    PyString::new(*py, unsafe {
        str::from_utf8_unchecked(slice::from_raw_parts(data, len))
    })
    .as_ref()
}

/// # Safety
/// TODO
#[export_name = "componentize-py#Init"]
pub unsafe extern "C" fn componentize_py_init<'a>(
    py: &'a Python<'a>,
    ty: usize,
    data: *const &'a PyAny,
    len: usize,
) -> &'a PyAny {
    match &TYPES.get().unwrap()[ty] {
        Type::Record { constructor, .. } => constructor
            .call1(*py, PyTuple::new(*py, slice::from_raw_parts(data, len)))
            .unwrap()
            .into_ref(*py),
        Type::Variant { cases, .. } => {
            assert!(len == 2);
            let discriminant =
                ptr::read(data.offset(isize::try_from(DISCRIMINANT_FIELD_INDEX).unwrap()))
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
            .into_ref(*py)
        }
        Type::Enum { constructor, count } => {
            assert!(len == 2);
            let discriminant =
                ptr::read(data.offset(isize::try_from(DISCRIMINANT_FIELD_INDEX).unwrap()))
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
                .into_ref(*py)
        }
        Type::RawUnion {
            types_to_discriminants,
            other_discriminant,
        } => {
            assert!(len == 2);
            let discriminant =
                ptr::read(data.offset(isize::try_from(DISCRIMINANT_FIELD_INDEX).unwrap()))
                    .extract::<usize>()
                    .unwrap();
            assert!(
                discriminant
                    < types_to_discriminants.as_ref(*py).len()
                        + other_discriminant.map(|_| 1).unwrap_or(0)
            );
            ptr::read(data.offset(isize::try_from(PAYLOAD_FIELD_INDEX).unwrap()))
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
                            .map(|&v| mem::transmute::<i32, u32>(v.extract().unwrap()))
                            .collect(),
                    ),),
                )
                .unwrap()
                .into_ref(*py)
        }
        Type::Option => {
            assert!(len == 2);
            let discriminant =
                ptr::read(data.offset(isize::try_from(DISCRIMINANT_FIELD_INDEX).unwrap()))
                    .extract::<u32>()
                    .unwrap();

            match discriminant {
                0 => py.None().into_ref(*py),
                1 => ptr::read(data.offset(isize::try_from(PAYLOAD_FIELD_INDEX).unwrap())),

                _ => unreachable!(),
            }
        }
        Type::NestingOption => {
            assert!(len == 2);
            let discriminant =
                ptr::read(data.offset(isize::try_from(DISCRIMINANT_FIELD_INDEX).unwrap()))
                    .extract::<u32>()
                    .unwrap();

            match discriminant {
                0 => py.None().into_ref(*py),

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
                    .into_ref(*py),

                _ => unreachable!(),
            }
        }
        Type::Result => {
            assert!(len == 2);
            let discriminant =
                ptr::read(data.offset(isize::try_from(DISCRIMINANT_FIELD_INDEX).unwrap()))
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
            .into_ref(*py)
        }
        Type::Tuple(length) => {
            assert!(*length == len);
            PyTuple::new(*py, slice::from_raw_parts(data, len))
        }
    }
}

#[export_name = "componentize-py#MakeList"]
pub extern "C" fn componentize_py_make_list<'a>(py: &'a Python) -> &'a PyList {
    PyList::empty(*py)
}

#[export_name = "componentize-py#ListAppend"]
pub extern "C" fn componentize_py_list_append(_py: &Python, list: &PyList, element: &PyAny) {
    list.append(element).unwrap();
}

#[export_name = "componentize-py#None"]
pub extern "C" fn componentize_py_none<'a>(py: &'a Python) -> &'a PyAny {
    py.None().into_ref(*py)
}

/// # Safety
/// TODO
#[export_name = "componentize-py#GetBytes"]
pub unsafe extern "C" fn componentize_py_get_bytes(
    _py: &Python,
    src: &PyBytes,
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
    py: &'a Python,
    src: *const u8,
    len: usize,
) -> &'a PyAny {
    PyBytes::new_with(*py, len, |dst| {
        dst.copy_from_slice(slice::from_raw_parts(src, len));
        Ok(())
    })
    .unwrap()
}

/// # Safety
/// TODO
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

/// # Safety
/// TODO
#[export_name = "cabi_export_realloc"]
pub unsafe extern "C" fn cabi_export_realloc(
    old_ptr: *mut u8,
    old_len: usize,
    align: usize,
    new_size: usize,
) -> *mut u8 {
    cabi_realloc(old_ptr, old_len, align, new_size)
}

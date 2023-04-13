#![deny(warnings)]

use {
    anyhow::{Error, Result},
    componentize_py_shared::{self as symbols, Symbols},
    heck::{ToSnakeCase, ToUpperCamelCase},
    once_cell::sync::OnceCell,
    pyo3::{
        exceptions::PyAssertionError,
        types::{PyList, PyMapping, PyModule, PyString, PyTuple},
        Py, PyAny, PyErr, PyObject, PyResult, Python, ToPyObject,
    },
    std::{
        alloc::{self, Layout},
        env,
        ffi::c_void,
        fs,
        mem::{self, MaybeUninit},
        ops::Deref,
        ptr, slice, str,
    },
};

static EXPORTS: OnceCell<Vec<PyObject>> = OnceCell::new();
static TYPES: OnceCell<Vec<Type>> = OnceCell::new();
static ENVIRON: OnceCell<Py<PyMapping>> = OnceCell::new();

#[derive(Debug)]
enum Type {
    Owned {
        constructor: PyObject,
        fields: Vec<String>,
    },
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

#[link(wasm_import_module = "componentize-py")]
extern "C" {
    #[cfg_attr(target_arch = "wasm32", link_name = "dispatch")]
    fn dispatch(context: *const c_void, input: *const c_void, output: *mut c_void, index: u32);
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
        dispatch(
            &module.py() as *const _ as _,
            params.as_ptr() as _,
            results.as_mut_ptr() as _,
            index,
        );

        // todo: is this sound, or do we need to `.into_iter().map(MaybeUninit::assume_init).collect()` instead?
        //
        // todo also: turn `result::err` results into exceptions, either here or in the generated Python code
        Ok(mem::transmute::<Vec<MaybeUninit<&PyAny>>, Vec<&PyAny>>(
            results,
        ))
    }
}

#[pyo3::pymodule]
#[pyo3(name = "componentize_py")]
fn componentize_py_module(_py: Python<'_>, module: &PyModule) -> PyResult<()> {
    module.add_function(pyo3::wrap_pyfunction!(call_import, module)?)
}

fn do_init() -> Result<()> {
    let symbols = fs::read(env::var("COMPONENTIZE_PY_SYMBOLS_PATH")?)?;
    let symbols = bincode::deserialize::<Symbols<'_>>(&symbols)?;

    pyo3::append_to_inittab!(componentize_py_module);

    pyo3::prepare_freethreaded_python();

    Python::with_gil(|py| {
        let app = py.import(env::var("COMPONENTIZE_PY_APP_NAME")?.deref())?;

        // TODO: do name tweaking in componentize-py instead of here so we don't have to pull in the heck
        // dependency
        EXPORTS
            .set(
                symbols
                    .exports
                    .iter()
                    .map(|function| {
                        let full_name = if let Some(interface) = function.interface {
                            format!(
                                "{}_{}",
                                interface.to_snake_case(),
                                function.name.to_snake_case()
                            )
                        } else {
                            function.name.to_snake_case()
                        };

                        Ok(app.getattr(full_name.as_str())?.into())
                    })
                    .collect::<PyResult<_>>()?,
            )
            .unwrap();

        TYPES
            .set(
                symbols
                    .types
                    .iter()
                    .enumerate()
                    .map(|(index, ty)| {
                        Ok(match ty {
                            symbols::Type::Owned(ty) => Type::Owned {
                                constructor: py
                                    .import(ty.interface)?
                                    .getattr(
                                        if let Some(name) = ty.name {
                                            name.to_upper_camel_case()
                                        } else {
                                            format!("AnonymousType{index}")
                                        }
                                        .as_str(),
                                    )?
                                    .into(),

                                fields: ty.fields.iter().map(|&f| f.to_owned()).collect(),
                            },

                            symbols::Type::Tuple(length) => Type::Tuple(*length),
                        })
                    })
                    .collect::<PyResult<_>>()?,
            )
            .unwrap();

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

#[export_name = "wizer.initialize"]
pub extern "C" fn init() {
    run_ctors();

    do_init().unwrap();
}

/// # Safety
/// TODO
#[export_name = "componentize-py#Dispatch"]
pub unsafe extern "C" fn componentize_py_dispatch(
    export: usize,
    lift: u32,
    lower: u32,
    param_count: u32,
    params: *const c_void,
    results: *mut c_void,
) {
    run_ctors();

    Python::with_gil(|py| {
        let mut params_lifted =
            vec![MaybeUninit::<&PyAny>::uninit(); param_count.try_into().unwrap()];

        dispatch(
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

        // todo: instead of unwrapping the result, return an `err` if the export function return type is `result`
        //
        // todo also: do a runtime type check to verify the result type matches the function return type.  What
        // should we do if it doesn't?  Abort?
        let result = EXPORTS.get().unwrap()[export]
            .call1(py, PyTuple::new(py, params_lifted))
            .unwrap();

        let result = result.into_ref(py);
        let result_array = [result];

        dispatch(
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
#[export_name = "cabi_realloc"]
pub unsafe extern "C" fn cabi_realloc(
    old_ptr: *mut u8,
    old_len: usize,
    align: usize,
    new_size: usize,
) -> *mut u8 {
    assert!(old_ptr == ptr::null_mut());
    assert!(old_len == 0);

    alloc::alloc(Layout::from_size_align(new_size, align).unwrap())
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
    _py: &'a Python,
    value: &'a PyAny,
    ty: usize,
    field: usize,
) -> &'a PyAny {
    match &TYPES.get().unwrap()[ty] {
        Type::Owned { fields, .. } => value.getattr(fields[field].as_str()),
        Type::Tuple(length) => {
            assert!(field < *length);
            value.downcast::<PyTuple>().unwrap().get_item(field)
        }
    }
    .unwrap()
}

#[export_name = "componentize-py#GetListLength"]
pub extern "C" fn componentize_py_get_list_length(_py: &Python, value: &PyAny) -> usize {
    value.downcast::<PyList>().unwrap().len()
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
    data: *const &PyAny,
    len: usize,
) -> &'a PyAny {
    match &TYPES.get().unwrap()[ty] {
        Type::Owned { constructor, .. } => constructor
            .call1(
                *py,
                PyTuple::new(*py, unsafe { slice::from_raw_parts(data, len) }),
            )
            .unwrap()
            .into_ref(*py),

        Type::Tuple(length) => {
            assert!(*length == len);
            PyTuple::new(*py, unsafe { slice::from_raw_parts(data, len) })
        }
    }
}

#[export_name = "componentize-py#MakeList"]
pub extern "C" fn componentize_py_make_list<'a>(py: &'a Python) -> &'a PyList {
    PyList::empty(*py)
}

#[export_name = "componentize-py#ListAppend"]
pub extern "C" fn componentize_py_list_append(_py: &Python, list: &PyList, element: &PyAny) {
    assert!(list.len() < 200); // temporary, for debugging; remove this
    list.append(element).unwrap();
}

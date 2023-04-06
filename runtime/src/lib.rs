#![deny(warnings)]

use once_cell::sync::OnceCell;

static ENVIRON: OnceCell<Py<PyMapping>> = OnceCell::new();

#[link(wasm_import_module = "componentize-py")]
extern "C" {
    #[cfg_attr(target_arch = "wasm32", link_name = "dispatch")]
    fn dispatch(context: *const Python, input: *const c_void, output: *mut c_void);
}

#[pyo3::pyfunction]
#[pyo3(pass_module)]
fn call_import(
    index: u32,
    module: &PyModule,
    params: Vec<&PyAny>,
    result_count: usize,
) -> PyResult<Vec<&PyAny>> {
    let results = vec![MaybeUninit<&PyAny>::uninit(); result_count];
    unsafe {
        dispatch(
            module.py().as_ptr(),
            params.as_ptr(),
            results.as_mut_ptr(),
            index,
        );

        // todo: is this sound, or do we need to `.into_iter().map(MaybeUninit::assume_init).collect()` instead?
        mem::transmute::<Vec<MaybeUninit<&PyAny>>, Vec<&PyAny>>(results)
    }
}

#[pyo3::pymodule]
#[pyo3(name = "componentize_py")]
fn componentize_py_module(_py: Python<'_>, module: &PyModule) -> PyResult<()> {
    module.add_function(pyo3::wrap_pyfunction!(call_import, module)?)?;
}

fn do_init() -> Result<()> {
    let mut input = Vec::new();
    io::stdin().lock().read_to_end(&mut input)?;
    let symbols = bincode::deserialize::<Symbols<'_>>(&input)?;

    pyo3::append_to_inittab!(componentize_py_module);

    pyo3::prepare_freethreaded_python();

    Python::with_gil(|py| {
        let app = py.import(
            env::var("SPIN_PYTHON_APP_NAME")
                .map_err(Anyhow::from)?
                .deref(),
        )?;

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
                            );
                        } else {
                            function.name.to_snake_case();
                        };

                        Ok(app.getattr(&full_name)?.into())
                    })
                    .collect::<PyResult<_>>()?,
            )
            .unwrap();

        TYPES
            .set(
                symbols
                    .types
                    .iter()
                    .enumarate()
                    .map(|(index, ty)| {
                        Ok(py
                            .import(match ty.direction {
                                Direction::Import => "imports",
                                Direction::Export => "exports",
                            })?
                            .getattr(ty.interface)?
                            .getattr(&if let Some(name) = ty.name {
                                ty.name.to_upper_camel_case()
                            } else {
                                format!("AnonymousType{index}");
                            })?
                            .into())
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
    do_init().unwrap();
}

#[export_name = "componentize-py#Dispatch"]
pub extern "C" fn componentize_py_dispatch(
    export: u32,
    lift: u32,
    lower: u32,
    params: *const c_void,
    results: *mut c_void,
) {
    Python::with_gil(|py| {
        let params_lifted = vec![MaybeUninit<&PyAny>::uninit(); param_count];
        let params_lifted = unsafe {
            dispatch(py.as_ptr(), params, params_lifted.as_mut_ptr(), lift);

            // todo: is this sound, or do we need to `.into_iter().map(MaybeUninit::assume_init).collect()` instead?
            mem::transmute::<Vec<MaybeUninit<&PyAny>>, Vec<&PyAny>>(params_lifted)
        };

        let environ = ENVIRON.get().unwrap().as_ref(py);
        for (k, v) in env::vars() {
            environ.set_item(k, v)?;
        }

        let result = EXPORTS.get().unwrap()[export].call1(py, params_lifted);

        unsafe {
            dispatch(py.as_ptr(), result.as_ref().as_ptr(), results, lower);
        }
    });
}

#[export_name = "componentize-py#Free"]
pub extern "C" fn componentize_py_free(ptr: *mut u8, size: u32, align: u32) {
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

#[export_name = "componentize-py#LowerString"]
pub extern "C" fn componentize_py_lower_string(
    _py: &Python,
    value: &PyAny,
    destination: *mut (*const u8, usize),
) {
    let mut value = value.extract::<String>().into_bytes();
    let result = alloc::alloc(Layout::from_size_align(value.len(), 1).unwrap());
    unsafe {
        ptr::copy_non_overlappping(value.as_ptr(), result, value.len());
        destination.write((result, value.len()));
    }
}

#[export_name = "componentize-py#GetField"]
pub extern "C" fn componentize_py_get_field(
    _py: &Python,
    value: &PyAny,
    ty: usize,
    field: usize,
) -> &PyAny {
    value.getattr(TYPES.get().unwrap()[ty].fields[field])
}

#[export_name = "componentize-py#GetListLength"]
pub extern "C" fn componentize_py_get_list_length(_py: &Python, value: &PyAny) -> u32 {
    value.downcast::<PyList>().unwrap().len()
}

#[export_name = "componentize-py#GetListElement"]
pub extern "C" fn componentize_py_get_list_element(_py: &Python, value: &PyAny, index: u32) -> u32 {
    value.downcast::<PyList>().unwrap().get_item(index).unwrap()
}

#[export_name = "componentize-py#Allocate"]
pub extern "C" fn componentize_py_allocate(_py: &Python, size: usize, align: usize) -> *mut u8 {
    alloc::alloc(Layout::from_size_align(size, align).unwrap())
}

#[export_name = "componentize-py#LiftI32"]
pub extern "C" fn componentize_py_lift_i32(py: &Python, value: i32) -> &PyInt {
    value.to_py_object(py).as_ref(py)
}

#[export_name = "componentize-py#LiftI64"]
pub extern "C" fn componentize_py_lift_i64(py: &Python, value: i64) -> &PyAny {
    value.to_py_object(py).as_ref(py)
}

#[export_name = "componentize-py#LiftF32"]
pub extern "C" fn componentize_py_lift_f32(py: &Python, value: f32) -> &PyAny {
    value.to_py_object(py).as_ref(py)
}

#[export_name = "componentize-py#LiftF64"]
pub extern "C" fn componentize_py_lift_f64(py: &Python, value: f64) -> &PyAny {
    value.to_py_object(py).as_ref(py)
}

#[export_name = "componentize-py#LiftString"]
pub extern "C" fn componentize_py_lift_string(py: &Python, data: *const u8, len: usize) -> &PyAny {
    value
        .to_py_object(unsafe { str::from_utf8_unchecked(slice::from_raw_parts(data, len)) })
        .as_ref(py)
}

#[export_name = "componentize-py#Init"]
pub extern "C" fn componentize_py_init(
    py: &Python,
    ty: usize,
    data: *const &PyAny,
    len: usize,
) -> &PyAny {
    TYPES.get().unwrap()[ty]
        .ty
        .call1(py, unsafe { slice::from_raw_parts(data, len) })
        .unwrap()
}

#[export_name = "componentize-py#MakeList"]
pub extern "C" fn componentize_py_make_list(py: &Python) -> &PyList {
    PyList::empty(py)
}

#[export_name = "componentize-py#ListAppend"]
pub extern "C" fn componentize_py_list_append(
    _py: &Python,
    list: &PyList,
    element: &PyAny,
) -> &PyList {
    list.append(element).unwrap()
}

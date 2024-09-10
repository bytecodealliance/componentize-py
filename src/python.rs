use {
    pyo3::{
        exceptions::PyAssertionError,
        pybacked::PyBackedStr,
        types::{PyAnyMethods, PyModule, PyModuleMethods},
        Bound, PyResult, Python,
    },
    std::{ffi::OsString, path::PathBuf},
    tokio::runtime::Runtime,
};

#[allow(clippy::too_many_arguments)]
#[pyo3::pyfunction]
#[pyo3(name = "componentize")]
#[pyo3(signature = (wit_path, world, python_path, module_worlds, app_name, output_path, stub_wasi))]
fn python_componentize(
    wit_path: Option<PathBuf>,
    world: Option<&str>,
    python_path: Vec<PyBackedStr>,
    module_worlds: Vec<(PyBackedStr, PyBackedStr)>,
    app_name: &str,
    output_path: PathBuf,
    stub_wasi: bool,
) -> PyResult<()> {
    (|| {
        Runtime::new()?.block_on(crate::componentize(
            wit_path.as_deref(),
            world,
            &python_path.iter().map(|s| &**s).collect::<Vec<_>>(),
            &module_worlds
                .iter()
                .map(|(a, b)| (&**a, &**b))
                .collect::<Vec<_>>(),
            app_name,
            &output_path,
            None,
            stub_wasi,
        ))
    })()
    .map_err(|e| PyAssertionError::new_err(format!("{e:?}")))
}

#[pyo3::pyfunction]
#[pyo3(name = "generate_bindings")]
#[pyo3(signature = (wit_path, world, world_module, output_dir))]
fn python_generate_bindings(
    wit_path: PathBuf,
    world: Option<&str>,
    world_module: Option<&str>,
    output_dir: PathBuf,
) -> PyResult<()> {
    crate::generate_bindings(&wit_path, world, world_module, &output_dir)
        .map_err(|e| PyAssertionError::new_err(format!("{e:?}")))
}

#[pyo3::pyfunction]
#[pyo3(name = "script")]
fn python_script(py: Python) -> PyResult<()> {
    crate::command::run(
        py.import_bound("sys")?
            .getattr("argv")?
            .extract::<Vec<OsString>>()?,
    )
    .map_err(|e| PyAssertionError::new_err(format!("{e:?}")))
}

#[pyo3::pymodule]
fn componentize_py(_py: Python, module: &Bound<PyModule>) -> PyResult<()> {
    module.add_function(pyo3::wrap_pyfunction!(python_componentize, module)?)?;
    module.add_function(pyo3::wrap_pyfunction!(python_generate_bindings, module)?)?;
    module.add_function(pyo3::wrap_pyfunction!(python_script, module)?)?;

    Ok(())
}

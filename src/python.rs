use {
    pyo3::{exceptions::PyAssertionError, types::PyModule, PyResult, Python},
    std::{ffi::OsString, path::PathBuf},
};

#[pyo3::pyfunction]
#[pyo3(name = "componentize")]
#[pyo3(signature = (wit_path, world, python_path, app_name, stub_wasi, output_path))]
fn python_componentize(
    wit_path: PathBuf,
    world: Option<&str>,
    python_path: &str,
    app_name: &str,
    stub_wasi: bool,
    output_path: PathBuf,
) -> PyResult<()> {
    crate::componentize(
        &wit_path,
        world,
        python_path,
        app_name,
        stub_wasi,
        &output_path,
    )
    .map_err(|e| PyAssertionError::new_err(format!("{e:?}")))
}

#[pyo3::pyfunction]
#[pyo3(name = "generate_bindings")]
#[pyo3(signature = (wit_path, world, output_dir))]
fn python_generate_bindings(
    wit_path: PathBuf,
    world: Option<&str>,
    output_dir: PathBuf,
) -> PyResult<()> {
    crate::generate_bindings(&wit_path, world, &output_dir)
        .map_err(|e| PyAssertionError::new_err(format!("{e:?}")))
}

#[pyo3::pyfunction]
#[pyo3(name = "script")]
fn python_script(py: Python) -> PyResult<()> {
    crate::command::run(
        py.import("sys")?
            .getattr("argv")?
            .extract::<Vec<OsString>>()?,
    )
    .map_err(|e| PyAssertionError::new_err(format!("{e:?}")))
}

#[pyo3::pymodule]
fn componentize_py(_py: Python, module: &PyModule) -> PyResult<()> {
    module.add_function(pyo3::wrap_pyfunction!(python_componentize, module)?)?;
    module.add_function(pyo3::wrap_pyfunction!(python_generate_bindings, module)?)?;
    module.add_function(pyo3::wrap_pyfunction!(python_script, module)?)?;

    Ok(())
}

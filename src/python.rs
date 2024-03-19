use {
    pyo3::{exceptions::PyAssertionError, types::PyModule, PyResult, Python},
    std::{ffi::OsString, path::PathBuf},
    tokio::runtime::Runtime,
};

#[allow(clippy::too_many_arguments)]
#[pyo3::pyfunction]
#[pyo3(name = "componentize")]
#[pyo3(signature = (wit_path, world, python_path, module_worlds, app_name, output_path, isyswasfa, stub_wasi))]
fn python_componentize(
    wit_path: Option<PathBuf>,
    world: Option<&str>,
    python_path: Vec<&str>,
    module_worlds: Vec<(&str, &str)>,
    app_name: &str,
    output_path: PathBuf,
    isyswasfa: Option<&str>,
    stub_wasi: bool,
) -> PyResult<()> {
    (|| {
        Runtime::new()?.block_on(crate::componentize(
            wit_path.as_deref(),
            world,
            &python_path,
            &module_worlds,
            app_name,
            &output_path,
            None,
            isyswasfa,
            stub_wasi,
        ))
    })()
    .map_err(|e| PyAssertionError::new_err(format!("{e:?}")))
}

#[pyo3::pyfunction]
#[pyo3(name = "generate_bindings")]
#[pyo3(signature = (wit_path, world, world_module, output_dir, isyswasfa))]
fn python_generate_bindings(
    wit_path: PathBuf,
    world: Option<&str>,
    world_module: Option<&str>,
    output_dir: PathBuf,
    isyswasfa: Option<&str>,
) -> PyResult<()> {
    crate::generate_bindings(&wit_path, world, world_module, &output_dir, isyswasfa)
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

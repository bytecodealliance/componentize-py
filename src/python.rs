#![allow(
    clippy::useless_conversion,
    reason = "some pyo3 macros produce code that does this"
)]

use {
    pyo3::{
        Bound, PyResult, Python,
        exceptions::PyAssertionError,
        pybacked::PyBackedStr,
        types::{PyAnyMethods, PyModule, PyModuleMethods},
    },
    std::{ffi::OsString, path::PathBuf},
    tokio::runtime::Runtime,
};

#[allow(clippy::too_many_arguments)]
#[pyo3::pyfunction]
#[pyo3(name = "componentize")]
#[pyo3(signature = (wit_path, world, features, all_features, world_module, python_path, module_worlds, app_name, output_path, stub_wasi, import_interface_names, export_interface_names))]
fn python_componentize(
    wit_path: Vec<PathBuf>,
    world: Option<&str>,
    features: Vec<String>,
    all_features: bool,
    world_module: Option<&str>,
    python_path: Vec<PyBackedStr>,
    module_worlds: Vec<(PyBackedStr, PyBackedStr)>,
    app_name: &str,
    output_path: PathBuf,
    stub_wasi: bool,
    import_interface_names: Vec<(PyBackedStr, PyBackedStr)>,
    export_interface_names: Vec<(PyBackedStr, PyBackedStr)>,
) -> PyResult<()> {
    (|| {
        Runtime::new()?.block_on(crate::componentize(
            &wit_path,
            world,
            &features,
            all_features,
            world_module,
            &python_path.iter().map(|s| s.as_ref()).collect::<Vec<_>>(),
            &module_worlds
                .iter()
                .map(|(a, b)| (a.as_ref(), b.as_ref()))
                .collect::<Vec<_>>(),
            app_name,
            &output_path,
            None,
            stub_wasi,
            &import_interface_names
                .iter()
                .map(|(a, b)| (a.as_ref(), b.as_ref()))
                .collect(),
            &export_interface_names
                .iter()
                .map(|(a, b)| (a.as_ref(), b.as_ref()))
                .collect(),
        ))
    })()
    .map_err(|e| PyAssertionError::new_err(format!("{e:?}")))
}

#[allow(clippy::too_many_arguments)]
#[pyo3::pyfunction]
#[pyo3(name = "generate_bindings")]
#[pyo3(signature = (wit_path, world, features, all_features, world_module, output_dir, import_interface_names, export_interface_names))]
fn python_generate_bindings(
    wit_path: Vec<PathBuf>,
    world: Option<&str>,
    features: Vec<String>,
    all_features: bool,
    world_module: Option<&str>,
    output_dir: PathBuf,
    import_interface_names: Vec<(PyBackedStr, PyBackedStr)>,
    export_interface_names: Vec<(PyBackedStr, PyBackedStr)>,
) -> PyResult<()> {
    crate::generate_bindings(
        &wit_path,
        world,
        &features,
        all_features,
        world_module,
        &output_dir,
        &import_interface_names
            .iter()
            .map(|(a, b)| (a.as_ref(), b.as_ref()))
            .collect(),
        &export_interface_names
            .iter()
            .map(|(a, b)| (a.as_ref(), b.as_ref()))
            .collect(),
    )
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
fn componentize_py(_py: Python, module: Bound<PyModule>) -> PyResult<()> {
    module.add_function(pyo3::wrap_pyfunction!(python_componentize, &module)?)?;
    module.add_function(pyo3::wrap_pyfunction!(python_generate_bindings, &module)?)?;
    module.add_function(pyo3::wrap_pyfunction!(python_script, &module)?)?;

    Ok(())
}

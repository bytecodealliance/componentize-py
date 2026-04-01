#![allow(
    clippy::useless_conversion,
    reason = "some pyo3 macros produce code that does this"
)]

use {
    crate::{BindingsGenerator, ComponentGenerator},
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
#[pyo3(signature = (wit_path, worlds, features, all_features, world_module, python_path, module_worlds, app_name, output_path, stub_wasi, import_interface_names, export_interface_names, full_names))]
fn python_componentize(
    wit_path: Vec<PathBuf>,
    worlds: Vec<String>,
    features: Vec<String>,
    all_features: bool,
    world_module: Option<&str>,
    python_path: Vec<PyBackedStr>,
    module_worlds: Vec<(PyBackedStr, Vec<PyBackedStr>)>,
    app_name: &str,
    output_path: PathBuf,
    stub_wasi: bool,
    import_interface_names: Vec<(PyBackedStr, PyBackedStr)>,
    export_interface_names: Vec<(PyBackedStr, PyBackedStr)>,
    full_names: bool,
) -> PyResult<()> {
    (|| {
        Runtime::new()?.block_on(
            ComponentGenerator {
                wit_path: &wit_path.iter().map(|v| v.as_path()).collect::<Vec<_>>(),
                worlds: &worlds.iter().map(|v| v.as_str()).collect::<Vec<_>>(),
                features: &features.iter().map(|v| v.as_str()).collect::<Vec<_>>(),
                all_features,
                world_module,
                python_path: &python_path.iter().map(|s| s.as_ref()).collect::<Vec<_>>(),
                module_worlds: &module_worlds
                    .iter()
                    .map(|(a, b)| (a.as_ref(), b.iter().map(|v| v.as_ref()).collect::<Vec<_>>()))
                    .collect::<Vec<_>>()
                    .iter()
                    .map(|(k, v)| (*k, v as &[_]))
                    .collect::<Vec<_>>(),
                app_name,
                output_path: &output_path,
                add_to_linker: None,
                stub_wasi,
                import_interface_names: &import_interface_names
                    .iter()
                    .map(|(a, b)| (a.as_ref(), b.as_ref()))
                    .collect(),
                export_interface_names: &export_interface_names
                    .iter()
                    .map(|(a, b)| (a.as_ref(), b.as_ref()))
                    .collect(),
                full_names,
            }
            .generate(),
        )
    })()
    .map_err(|e| PyAssertionError::new_err(format!("{e:?}")))
}

#[allow(clippy::too_many_arguments)]
#[pyo3::pyfunction]
#[pyo3(name = "generate_bindings")]
#[pyo3(signature = (wit_path, worlds, features, all_features, world_module, output_dir, import_interface_names, export_interface_names, full_names))]
fn python_generate_bindings(
    wit_path: Vec<PathBuf>,
    worlds: Vec<String>,
    features: Vec<String>,
    all_features: bool,
    world_module: Option<&str>,
    output_dir: PathBuf,
    import_interface_names: Vec<(PyBackedStr, PyBackedStr)>,
    export_interface_names: Vec<(PyBackedStr, PyBackedStr)>,
    full_names: bool,
) -> PyResult<()> {
    BindingsGenerator {
        wit_path: &wit_path.iter().map(|v| v.as_path()).collect::<Vec<_>>(),
        worlds: &worlds.iter().map(|v| v.as_str()).collect::<Vec<_>>(),
        features: &features.iter().map(|v| v.as_str()).collect::<Vec<_>>(),
        all_features,
        world_module,
        output_dir: &output_dir,
        import_interface_names: &import_interface_names
            .iter()
            .map(|(a, b)| (a.as_ref(), b.as_ref()))
            .collect(),
        export_interface_names: &export_interface_names
            .iter()
            .map(|(a, b)| (a.as_ref(), b.as_ref()))
            .collect(),
        full_names,
    }
    .generate()
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

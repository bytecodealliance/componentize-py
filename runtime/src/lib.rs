#![deny(warnings)]
#![allow(
    clippy::useless_conversion,
    reason = "some pyo3 macros produce code that does this"
)]
#![allow(
    static_mut_refs,
    reason = "wit-bindgen::generate produces code that does this"
)]
#![allow(unknown_lints)]
#![allow(
    unnecessary_transmutes,
    reason = "nightly warning but not supported on stable"
)]
#![allow(
    clippy::not_unsafe_ptr_arg_deref,
    reason = "wit_dylib_ffi::export produces code that does this"
)]

use {
    anyhow::{Error, Result},
    exports::exports::{
        self as exp, Constructor, FunctionExportKind, Guest, OptionKind, ResultRecord, ReturnStyle,
        Static, Symbols,
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
        mem,
        ops::DerefMut,
        slice, str,
        sync::{Mutex, Once},
    },
    wasi::cli::environment,
    wit_dylib_ffi::{
        self as wit, Call, ExportFunction, Interpreter, List, Type, Wit, WitOption, WitResult,
    },
};

wit_bindgen::generate!({
    world: "init",
    path: "../wit",
    generate_all,
});

export!(MyExports);

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

static BORROWS: Mutex<Vec<Borrow>> = Mutex::new(Vec::new());

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

#[pyo3::pyfunction]
#[pyo3(pass_module)]
fn call_import<'a>(
    module: Bound<'a, PyModule>,
    index: u32,
    params: Vec<Bound<'a, PyAny>>,
    _result_count: usize,
) -> PyResult<Vec<Bound<'a, PyAny>>> {
    let func = WIT
        .get()
        .unwrap()
        .import_func(usize::try_from(index).unwrap());

    if func.is_async() {
        todo!()
    } else {
        let mut call = MyCall::new(params.into_iter().rev().map(|v| v.unbind()).collect());
        func.call_import_sync(&mut call);
        Ok(mem::take(&mut call.stack)
            .into_iter()
            .map(|v| v.into_bound(module.py()))
            .collect())
    }
}

#[pyo3::pyfunction]
#[pyo3(pass_module)]
fn drop_resource(_module: &Bound<PyModule>, index: usize, handle: u32) -> PyResult<()> {
    unsafe {
        mem::transmute::<usize, unsafe extern "C" fn(u32)>(index)(handle);
    }
    Ok(())
}

#[pyo3::pymodule]
#[pyo3(name = "componentize_py_runtime")]
fn componentize_py_module(_py: Python<'_>, module: &Bound<PyModule>) -> PyResult<()> {
    module.add_function(pyo3::wrap_pyfunction!(call_import, module)?)?;
    module.add_function(pyo3::wrap_pyfunction!(drop_resource, module)?)
}

fn do_init(app_name: String, symbols: Symbols, stub_wasi: bool) -> Result<(), String> {
    pyo3::append_to_inittab!(componentize_py_module);

    Python::initialize();

    let init = |py: Python| {
        let app = match py.import(app_name.as_str()) {
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

        let types = py.import(symbols.types_package.as_str())?;

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

    Python::attach(|py| init(py).map_err(|e| format!("{e:?}")))
}

struct MyExports;

impl Guest for MyExports {
    fn init(app_name: String, symbols: Symbols, stub_wasi: bool) -> Result<(), String> {
        let result = do_init(app_name, symbols, stub_wasi);

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

struct MyInterpreter;

impl Interpreter for MyInterpreter {
    type CallCx<'a> = MyCall<'a>;

    fn initialize(wit: Wit) {
        WIT.set(wit).map_err(drop).unwrap();
    }

    fn export_start<'a>(_: Wit, _: ExportFunction) -> Box<MyCall<'a>> {
        Box::new(MyCall::new(Vec::new()))
    }

    fn export_call(_: Wit, func: ExportFunction, cx: &mut MyCall<'_>) {
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

            let borrows = mem::take(BORROWS.lock().unwrap().deref_mut());
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
        });
    }

    async fn export_call_async(_: Wit, func: ExportFunction, cx: Box<MyCall<'static>>) {
        _ = (func, cx);
        todo!()
    }

    fn resource_dtor(ty: wit::Resource, handle: usize) {
        _ = (ty, handle);
        todo!()
    }
}

struct MyCall<'a> {
    _phantom: PhantomData<&'a ()>,
    iter_stack: Vec<usize>,
    deferred_deallocations: Vec<(*mut u8, Layout)>,
    strings: Vec<String>,
    stack: Vec<Py<PyAny>>,
}

impl MyCall<'_> {
    fn new(stack: Vec<Py<PyAny>>) -> Self {
        // TODO: tell py03 to attach (and detach on drop) this thread to the
        // interpreter.
        Self {
            _phantom: PhantomData,
            iter_stack: Vec::new(),
            deferred_deallocations: Vec::new(),
            strings: Vec::new(),
            stack,
        }
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
                imported_resource_to_canon(py, value)
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
                value
                    .bind(py)
                    .getattr(intern!(py, "finalizer"))
                    .unwrap()
                    .call_method0(intern!(py, "detach"))
                    .unwrap();

                imported_resource_to_canon(py, value)
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

    fn pop_future(&mut self, ty: wit::Future) -> u32 {
        _ = ty;
        todo!()
    }

    fn pop_stream(&mut self, ty: wit::Stream) -> u32 {
        _ = ty;
        todo!()
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
        if let Type::U8 = ty.ty() {
            Python::attach(|py| {
                let src = self.stack.pop().unwrap();
                let src = src.cast_bound::<PyBytes>(py).unwrap();
                let len = src.len().unwrap();
                let dst = alloc::alloc(Layout::from_size_align(len, 1).unwrap());
                slice::from_raw_parts_mut(dst, len).copy_from_slice(src.as_bytes());
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

                BORROWS.lock().unwrap().push(Borrow {
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
        _ = (ty, handle);
        todo!()
    }

    fn push_stream(&mut self, ty: wit::Stream, handle: u32) {
        _ = (ty, handle);
        todo!()
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
        if let Type::U8 = ty.ty() {
            self.stack.push(Python::attach(|py| {
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

fn imported_resource_to_canon(py: Python<'_>, value: Py<PyAny>) -> u32 {
    value
        .bind(py)
        .getattr(intern!(py, "handle"))
        .unwrap()
        .extract()
        .unwrap()
}

wit_dylib_ffi::export!(MyInterpreter);

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

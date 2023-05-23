# componentize-py

This is a tool to convert a Python application to a [WebAssembly
component](https://github.com/WebAssembly/component-model).  It takes the
following as input:

- a [WIT](https://github.com/WebAssembly/component-model/blob/main/design/mvp/WIT.md) file or directory
- the name of a [WIT world](https://github.com/WebAssembly/component-model/blob/main/design/mvp/WIT.md#wit-worlds) defined in the above file or directory
- the name of a Python module which targets said world
- a list of directories in which to find the Python module and its dependencies

The output is a component which may be run using
e.g. [`wasmtime`](https://github.com/bytecodealliance/wasmtime).  See the
[tests](src/test) for examples.

## Installing from PyPI

```
pip install componentize-py
```

## Build Prerequisites

- [WASI SDK](https://github.com/WebAssembly/wasi-sdk) v16 (later versions may work, but have not yet been tested)
    - Install this to `/opt/wasi-sdk`, or else specify an alternative location via the `WASI_SDK_PATH` environment variable
- Tools needed to build [CPython](https://github.com/python/cpython) (e.g. Make, Clang, etc.)
- [Rust](https://rustup.rs/) v1.68 or later, including the `wasm32-wasi` and `wasm32-unknown-unkown` targets

## Building and Running

```shell
cargo run --release -- --help
```

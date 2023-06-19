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
[tests](https://github.com/dicej/componentize-py/tree/main/src/test) for examples.

For an example of running a sandboxed Python guest within a Python host, see
[component-sandbox-demo](https://github.com/dicej/component-sandbox-demo).

## Installing from PyPI

```shell
pip install componentize-py
```

## Building from source

For the time being, we use a temporary fork of WASI-SDK which enables both
shared library support and dlopen/dlsym.  Once those features are upstreamed,
we'll switch.  Specifically, the remaining patches are:

- https://github.com/WebAssembly/wasi-libc/pull/429
- https://github.com/WebAssembly/wasi-sdk/pull/338
- Additional, yet-to-be created PRs to enable dlopen/dlsym

### Prerequisites

- Tools needed to build [CPython](https://github.com/python/cpython) (Make, Clang, etc.)
- [Rust](https://rustup.rs/) stable 1.68 or later *and* nightly 2023-07-27 or later, including the `wasm32-wasi` and `wasm32-unknown-unknown` targets
  - Note that we currently use the `-Z build-std` Cargo option to build the `componentize-py` runtime with position-independent code (which is not the default for `wasm32-wasi`) and this requires using a recent nightly build of Rust.
  
For Rust, something like this should work once you have `rustup`:

```shell
rustup update
rustup install nightly
rustup component add rust-src --toolchain nightly
rustup target add wasm32-wasi wasm32-unknown-unknown
```

### Building and Running

First, make sure you've got all the submodules cloned.

```shell
git submodule update --init --recursive
```

Next, install WASI SDK to `/opt/wasi-sdk` (alternatively, you can specify an
alternative location and reference it later using the `WASI_SDK_PATH`
environment variable).  Replace `linux` with `macos` or `mingw` (Windows) below
depending on your OS.

```shell
curl -LO https://github.com/dicej/wasi-sdk/releases/download/shared-library-alpha-1/wasi-sdk-20.15ge8bb8fade354-linux.tar.gz
tar xf wasi-sdk-20.15ge8bb8fade354-linux.tar.gz
sudo mv wasi-sdk-20.15ge8bb8fade354 /opt/wasi-sdk
export WASI_SDK_PATH=/opt/wasi-sdk
```

Finally, build and run `componentize-py`.

```shell
cargo run --release -- --help
```

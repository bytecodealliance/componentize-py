# Example: `matrix-math`

This is an example of how to use [componentize-py] to build a CLI app that does
matrix multiplication using [NumPy] inside a sandboxed environment.  This
demonstrates using a non-trivial Python package containing native extensions
within a guest component.

[componentize-py]: https://github.com/bytecodealliance/componentize-py
[NumPy]: https://numpy.org

## Prerequisites

* `wasmtime` 18.0.0 or later
* `componentize-py` 0.13.5
* `NumPy`, built for WASI

Note that we use an unofficial build of NumPy since the upstream project does
not yet publish WASI builds.

Below, we use [Rust](https://rustup.rs/)'s `cargo` to install `Wasmtime`.  If
you don't have `cargo`, you can download and install from
https://github.com/bytecodealliance/wasmtime/releases/tag/v18.0.0.

```
cargo install --version 18.0.0 wasmtime-cli
pip install componentize-py==0.13.5
curl -OL https://github.com/dicej/wasi-wheels/releases/download/v0.0.1/numpy-wasi.tar.gz
tar xf numpy-wasi.tar.gz
```

## Running the demo

```
componentize-py -d ../../wit -w matrix-math componentize app -o matrix-math.wasm
wasmtime run matrix-math.wasm '[[1, 2], [4, 5], [6, 7]]' '[[1, 2, 3], [4, 5, 6]]'
```

The second command above should print the following:

```
matrix_multiply received arguments [[1, 2], [4, 5], [6, 7]] and [[1, 2, 3], [4, 5, 6]]
[[9, 12, 15], [24, 33, 42], [34, 47, 60]]
```

If you run into any problems, please file an issue!

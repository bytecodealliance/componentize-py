# `matrix-math`

This is an example of how to use [wasmtime-py] and [componentize-py] to do
matrix multiplication using [NumPy] inside a sandboxed WASI component.  This
demonstrates using a non-trivial Python package containing native extensions
within a guest component.

[wasmtime-py]: https://github.com/bytecodealliance/wasmtime-py
[componentize-py]: https://github.com/dicej/componentize-py
[NumPy]: https://numpy.org

## Prerequisites

* `wasmtime-py` 13 or later
* `componentize-py` 0.3.1 or later
* `NumPy`, built for WASI

Note that we must build `wasmtime-py` from source until version 13 has been
released.

Also note that we use an unofficial build of NumPy since the upstream project
does not yet publish WASI builds.

```
git clone https://github.com/bytecodealliance/wasmtime-py
(cd wasmtime-py && python ci/download-wasmtime.py && python ci/build-rust.py && pip install .)
pip install componentize-py
curl -OL https://github.com/dicej/wasi-wheels/releases/download/canary/numpy-wasi.tar.gz
tar xf numpy-wasi.tar.gz
```

## Running the demo

```
componentize-py -d wit -w matrix-math componentize guest -o matrix-math.wasm
python3 -m wasmtime.bindgen matrix-math.wasm --out-dir matrix_math
python3 host.py '[[1, 2], [4, 5], [6, 7]]' '[[1, 2, 3], [4, 5, 6]]'
```

The last command above should print the following:

```
guest log: matrix_multiply received arguments [[1.0, 2.0], [4.0, 5.0], [6.0, 7.0]] and [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]
result: [[9.0, 12.0, 15.0], [24.0, 33.0, 42.0], [34.0, 47.0, 60.0]]
```

If you run into any problems, please file an issue!

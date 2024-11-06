#!/bin/bash

set -euo pipefail

export COMPONENTIZE_PY_TEST_COUNT=0
export COMPONENTIZE_PY_TEST_SEED=bc6ad1950594f1fe477144ef5b3669dd5962e49de4f3b666e5cbf9072507749a
export WASMTIME_BACKTRACE_DETAILS=1

cargo build --release


# HTTP
# Just compiling for now
(cd examples/http \
    && ../../target/release/componentize-py -d ../../wit -w wasi:http/proxy@0.2.0 componentize app -o http.wasm)

# Matrix Math
(cd examples/matrix-math \
    && curl -OL https://github.com/dicej/wasi-wheels/releases/download/v0.0.1/numpy-wasi.tar.gz \
    && tar xf numpy-wasi.tar.gz \
    && ../../target/release/componentize-py -d ../../wit -w matrix-math componentize app -o matrix-math.wasm \
    && wasmtime run matrix-math.wasm '[[1, 2], [4, 5], [6, 7]]' '[[1, 2, 3], [4, 5, 6]]')

# Sandbox
(cd examples/sandbox \
    && ../../target/release/componentize-py -d sandbox.wit componentize --stub-wasi guest -o sandbox.wasm \
    && python -m wasmtime.bindgen sandbox.wasm --out-dir sandbox \
    && python host.py "2 + 2")

# TCP
# Just compiling for now
(cd examples/tcp \
    && ../../target/release/componentize-py -d ../../wit -w wasi:cli/command@0.2.0 componentize app -o tcp.wasm)

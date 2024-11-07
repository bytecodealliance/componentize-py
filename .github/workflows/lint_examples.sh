#!/bin/bash

set -euo pipefail

export COMPONENTIZE_PY_TEST_COUNT=0
export COMPONENTIZE_PY_TEST_SEED=bc6ad1950594f1fe477144ef5b3669dd5962e49de4f3b666e5cbf9072507749a
export WASMTIME_BACKTRACE_DETAILS=1

cargo build --release

# # Matrix Math
(cd examples/matrix-math \
    && rm -rf matrix_math || true \
    && curl -OL https://github.com/dicej/wasi-wheels/releases/download/v0.0.1/numpy-wasi.tar.gz \
    && tar xf numpy-wasi.tar.gz \
    && ../../target/release/componentize-py -d ../../wit -w matrix-math bindings . \
    && mypy --strict --follow-imports silent -m app -p matrix_math)

# Sandbox
(cd examples/sandbox \
    && rm -rf sandbox || true \
    && ../../target/release/componentize-py -d sandbox.wit bindings . \
    && mypy --strict -m guest -p sandbox)

# TCP
(cd examples/tcp \
    && rm -rf command || true \
    && ../../target/release/componentize-py -d ../../wit -w wasi:cli/command@0.2.0 bindings . \
    && mypy --strict .)

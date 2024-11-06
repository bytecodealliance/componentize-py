#!/bin/bash

set -euo pipefail

export COMPONENTIZE_PY_TEST_COUNT=0
export COMPONENTIZE_PY_TEST_SEED=bc6ad1950594f1fe477144ef5b3669dd5962e49de4f3b666e5cbf9072507749a
export WASMTIME_BACKTRACE_DETAILS=1

cargo build --release

# TCP
# Just compiling for now
(cd examples/tcp \
    && ../../target/release/componentize-py -d ../../wit -w wasi:cli/command@0.2.0 componentize app -o tcp.wasm)

#!/bin/bash

set -euo pipefail

cargo fmt --all -- --check
COMPONENTIZE_PY_TEST_COUNT=0 \
    COMPONENTIZE_PY_TEST_SEED=bc6ad1950594f1fe477144ef5b3669dd5962e49de4f3b666e5cbf9072507749a \
    cargo clippy --all-targets --all-features
(cd runtime \
 && cargo fmt --all -- --check \
 && PYO3_CONFIG_FILE=$(pwd)/pyo3-config-clippy.txt cargo clippy --all-targets --all-features)

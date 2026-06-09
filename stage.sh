#!/bin/bash

# This script generates a crate which may be published to e.g. crates.io.
#
# We unfortunately can't publish directly from the root of the repository due to
# the odd things `build.rs` does.  See the comment in `alt-build.rs` for
# details.

cargo clean
cargo build --release
mkdir -p target/staged
cp -a Cargo.toml Cargo.lock CONTRIBUTING.md examples LICENSE README.md runtime src tests test-generator wit target/staged/
cp alt-build.rs target/staged/build.rs
cp $(find target/release/build -name '*.zst') target/staged/

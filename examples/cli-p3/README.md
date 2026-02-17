# Example: `cli-p3`

This is an example of how to use [componentize-py] and [Wasmtime] to build and
run a Python-based component targetting version `0.3.0-rc-2026-01-06` of the
[wasi-cli] `command` world.

[componentize-py]: https://github.com/bytecodealliance/componentize-py
[Wasmtime]: https://github.com/bytecodealliance/wasmtime
[wasi-cli]: https://github.com/WebAssembly/wasi-cli

## Prerequisites

* `Wasmtime` 41.0.3
* `componentize-py` 0.21.0

Below, we use [Rust](https://rustup.rs/)'s `cargo` to install `Wasmtime`.  If
you don't have `cargo`, you can download and install from
https://github.com/bytecodealliance/wasmtime/releases/tag/v41.0.3.

```
cargo install --version 41.0.3 wasmtime-cli
pip install componentize-py==0.21.0
```

## Running the demo

```
componentize-py -d ../../wit -w wasi:cli/command@0.3.0-rc-2026-01-06 componentize app -o cli.wasm
wasmtime run -Sp3 -Wcomponent-model-async cli.wasm
```

The `wasmtime run` command above should print "Hello, world!".

# Example: `cli`

This is an example of how to use [componentize-py] and [Wasmtime] to build and
run a Python-based component targetting the [wasi-cli] `command` world.

[componentize-py]: https://github.com/bytecodealliance/componentize-py
[Wasmtime]: https://github.com/bytecodealliance/wasmtime
[wasi-cli]: https://github.com/WebAssembly/wasi-cli

## Prerequisites

* `Wasmtime` 17.0.0 or later
* `componentize-py` 0.11.2

Below, we use [Rust](https://rustup.rs/)'s `cargo` to install `Wasmtime`.  If
you don't have `cargo`, you can download and install from
https://github.com/bytecodealliance/wasmtime/releases/tag/v17.0.0.

```
cargo install --version 17.0.0 wasmtime-cli
pip install componentize-py==0.11.2
```

## Running the demo

```
componentize-py -d ../../wit -w wasi:cli/command@0.2.0 componentize app -o cli.wasm
wasmtime run --wasm component-model cli.wasm
```

The `wasmtime run` command above should print "Hello, world!".

# Example: `cli`

This is an example of how to use [componentize-py] and [Wasmtime] to build and
run a Python-based component targetting the [wasi-cli] `command` world.

Note that, as of this writing, `wasi-cli` has not yet stabilized.  Here we use a
snapshot, which may differ from later revisions.

[componentize-py]: https://github.com/bytecodealliance/componentize-py
[Wasmtime]: https://github.com/bytecodealliance/wasmtime
[wasi-cli]: https://github.com/WebAssembly/wasi-cli

## Prerequisites

* `Wasmtime` 15.0.1 (later versions may use a different, incompatible `wasi-cli` snapshot)
* `componentize-py` 0.7.1

Below, we use [Rust](https://rustup.rs/)'s `cargo` to install `Wasmtime` since,
as of this writing, 15.0.1 has not yet been released.  Once it has been
released, you'll be able to download and install from
https://github.com/bytecodealliance/wasmtime/releases/tag/v15.0.1.

```
cargo install --locked --git https://github.com/bytecodealliance/wasmtime --branch release-15.0.0 wasmtime-cli
pip install componentize-py
```

## Running the demo

```
componentize-py -d ../../wit -w wasi:cli/command@0.2.0-rc-2023-11-10 componentize app -o cli.wasm
wasmtime run --wasm component-model cli.wasm
```

The `wasmtime run` command above should print "Hello, world!".

# Example: `multithreading`

This is an example of how to use [componentize-py] and [Wasmtime] to build and
run a Python-based component targetting version `0.3.0` of the [wasi-cli]
`command` world and using multiple threads.

[componentize-py]: https://github.com/bytecodealliance/componentize-py
[Wasmtime]: https://github.com/bytecodealliance/wasmtime
[wasi-cli]: https://github.com/WebAssembly/WASI/tree/v0.3.0/proposals/cli/wit

## Prerequisites

* `Wasmtime` 48.0.0
* `componentize-py` 0.25.0

Below, we use [Rust](https://rustup.rs/)'s `cargo` to install `Wasmtime`.  If
you don't have `cargo`, you can download and install from
https://github.com/bytecodealliance/wasmtime/releases/tag/v48.0.0.

```
cargo install --version 48.0.0 wasmtime-cli
pip install componentize-py==0.25.0
```

## Running the demo

```
componentize-py -d ../../wit -w wasi:cli/command@0.3.0 componentize --target wasm32-wasip3-threads app -o app.wasm
wasmtime run -Wcomponent-model-threading app.wasm
```

The `wasmtime run` command above should print something like the following (the
output may vary depending on how the threads are scheduled):

```
thread `a` started
thread `b` started
thread `c` started
started all threads
thread `a` finished
thread `b` finished
thread `c` finished
joined all threads
```

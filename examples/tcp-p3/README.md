# Example: `tcp-p3`

This is an example of how to use [componentize-py] and [Wasmtime] to build and
run a Python-based component targetting version `0.3.0-rc-2026-03-15` of the
[wasi-cli] `command` world and making an outbound TCP request using [wasi-sockets].

[componentize-py]: https://github.com/bytecodealliance/componentize-py
[Wasmtime]: https://github.com/bytecodealliance/wasmtime
[wasi-cli]: https://github.com/WebAssembly/WASI/tree/v0.3.0-rc-2026-03-15/proposals/cli/wit-0.3.0-draft
[wasi-sockets]: https://github.com/WebAssembly/WASI/tree/v0.3.0-rc-2026-03-15/proposals/sockets/wit-0.3.0-draft

## Prerequisites

* `Wasmtime` 43.0.0
* `componentize-py` 0.23.0

Below, we use [Rust](https://rustup.rs/)'s `cargo` to install `Wasmtime`.  If
you don't have `cargo`, you can download and install from
https://github.com/bytecodealliance/wasmtime/releases/tag/v43.0.0.

```
cargo install --version 43.0.0 wasmtime-cli
pip install componentize-py==0.23.0
```

## Running the demo

First, in a separate terminal, run `netcat`, telling it to listen for incoming
TCP connections.  You can choose any port you like.

```
nc -l 127.0.0.1 3456
```

Now, build and run the example, using the same port you gave to `netcat`.

```
componentize-py -d ../../wit -w wasi:cli/command@0.3.0-rc-2026-03-15 componentize app -o tcp.wasm
wasmtime run -Sp3 -Sinherit-network -Wcomponent-model-async tcp.wasm 127.0.0.1:3456
```

The program will open a TCP connection, send a message, and wait to receive a
response before exiting.  You can give it a response by typing anything you like
into the terminal where `netcat` is running and then pressing the `Enter` key on
your keyboard.

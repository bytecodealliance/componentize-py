# Example: `tls-p3`

This is an example of how to use [componentize-py] and [Wasmtime] to build and
run a Python-based TLS client component targetting version `0.3.0-rc-2026-02-09`
of the [wasi-cli] `command` world with [wasi-tls] and [wasi-sockets] support.

[componentize-py]: https://github.com/bytecodealliance/componentize-py
[Wasmtime]: https://github.com/bytecodealliance/wasmtime
[wasi-cli]: https://github.com/WebAssembly/WASI/tree/v0.3.0-rc-2026-02-09/proposals/cli/wit-0.3.0-draft
[wasi-sockets]: https://github.com/WebAssembly/WASI/tree/v0.3.0-rc-2026-02-09/proposals/sockets/wit-0.3.0-draft
[wasi-tls]: https://github.com/WebAssembly/wasi-tls

## Prerequisites

* `Wasmtime` 43.0.0
* `componentize-py` 0.21.0

Below, we use [Rust](https://rustup.rs/)'s `cargo` to install `Wasmtime`.  If
you don't have `cargo`, you can download and install from
https://github.com/bytecodealliance/wasmtime/releases/tag/v43.0.0.

```
cargo install --version 43.0.0 wasmtime-cli
pip install componentize-py==0.21.0
```

## Running the demo

```
componentize-py -d ../../wit -w tls-p3 componentize app -o tls.wasm
wasmtime run -Sp3,inherit-network,tls,allow-ip-name-lookup -Wcomponent-model-async tls.wasm <server_name>
```

For example, to connect to `api.github.com` over TLS:

```
wasmtime run -Sp3,inherit-network,tls,allow-ip-name-lookup -Wcomponent-model-async tls.wasm api.github.com
```

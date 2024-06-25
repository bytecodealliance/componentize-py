# Example: `http`

This is an example of how to use [componentize-py] and [Wasmtime] to build and
run a Python-based component targetting the [wasi-http] `proxy` world.

[componentize-py]: https://github.com/bytecodealliance/componentize-py
[Wasmtime]: https://github.com/bytecodealliance/wasmtime
[wasi-http]: https://github.com/WebAssembly/wasi-http

## Prerequisites

* `Wasmtime` 18.0.0 or later
* `componentize-py` 0.13.5

Below, we use [Rust](https://rustup.rs/)'s `cargo` to install `Wasmtime`.  If
you don't have `cargo`, you can download and install from
https://github.com/bytecodealliance/wasmtime/releases/tag/v18.0.0.

```
cargo install --version 18.0.0 wasmtime-cli
pip install componentize-py==0.13.5
```

## Running the demo

First, build the app and run it:

```
componentize-py -d ../../wit -w wasi:http/proxy@0.2.0 componentize app -o http.wasm
wasmtime serve --wasi common http.wasm
```

Then, in another terminal, use cURL to send a request to the app:

```
curl -i -H 'content-type: text/plain' --data-binary @- http://127.0.0.1:8080/echo <<EOF
’Twas brillig, and the slithy toves
      Did gyre and gimble in the wabe:
All mimsy were the borogoves,
      And the mome raths outgrabe.
EOF
```

The above should echo the request body in the response.

In addition to the `/echo` endpoint, the app supports a `/hash-all` endpoint
which concurrently downloads one or more URLs and streams the SHA-256 hashes of
their contents.  You can test it with e.g.:

```
curl -i \
    -H 'url: https://webassembly.github.io/spec/core/' \
    -H 'url: https://www.w3.org/groups/wg/wasm/' \
    -H 'url: https://bytecodealliance.org/' \
    http://127.0.0.1:8080/hash-all
```

If you run into any problems, please file an issue!

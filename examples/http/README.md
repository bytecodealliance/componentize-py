# Example: `http`

This is an example of how to use [componentize-py] and [Spin] to build and run a
Python-based component targetting the [wasi-http] `proxy` world.

Note that, as of this writing, neither `wasi-http` nor the portions of
`wasi-cli` on which it is based have stablized.  Here we use a snapshot of both,
which may differ from later revisions.

[componentize-py]: https://github.com/dicej/componentize-py
[Spin]: https://github.com/fermyon/spin
[wasi-http]: https://github.com/WebAssembly/wasi-http

## Prerequisites

* `dicej/spin` branch `wasi-http`
* `componentize-py` 0.4.0
* `Rust`, for installing `Spin`

```
cargo install --locked --git https://github.com/dicej/spin --branch wasi-http
pip install componentize-py
```

## Running the demo

First, build the app and run it:

```
spin build --up
```

Then, in another terminal, use cURL to send a request to the app:

```
curl -i -H 'content-type: text/plain' --data-binary @- http://127.0.0.1:3000/echo <<EOF
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
    http://127.0.0.1:3000/hash-all
```

If you run into any problems, please file an issue!

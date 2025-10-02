## Contributing

Please file issues (bug reports, questions, feature requests, etc.) on [the
GitHub repository](https://github.com/bytecodealliance/componentize-py).  That's also the
place for pull requests.  If you're planning to make a big change, please file
an issue first to avoid duplicate effort.

Outside of GitHub, most development discussion happens at the [SIG Guest
Languages](https://github.com/bytecodealliance/meetings/tree/main/SIG-Guest-Languages)
[Python
subgroup](https://github.com/bytecodealliance/meetings/tree/main/SIG-Guest-Languages/Python)
meetings and the Guest Languages [Zulip
channel](https://bytecodealliance.zulipchat.com/#narrow/stream/394175-SIG-Guest-Languages).

## Building from source

### Prerequisites

- Tools needed to build [CPython](https://github.com/python/cpython) (Make, Clang, etc.)
- [Rust](https://rustup.rs/) stable 1.71 or later *and* nightly 2023-07-27 or later, including the `wasm32-wasip1` and `wasm32-unknown-unknown` targets
  - Note that we currently use the `-Z build-std` Cargo option to build the `componentize-py` runtime with position-independent code (which is not the default for `wasm32-wasip1`) and this requires using a recent nightly build of Rust.
  
For Rust, something like this should work once you have `rustup`:

```shell
rustup update
rustup install nightly
rustup component add rust-src --toolchain nightly
rustup target add wasm32-wasip1 wasm32-unknown-unknown
rustup target add --toolchain nightly wasm32-wasip1 wasm32-unknown-unknown
```

### Building and Running

First, make sure you've got all the submodules cloned.

```shell
git submodule update --init --recursive
```

Next, install WASI-SDK 27 to `/opt/wasi-sdk` (alternatively, you can specify a
different location and reference it later using the `WASI_SDK_PATH` environment
variable).  Replace `arm64-linux` with `x86_64-linux`, `arm64-macos`,
`x86_64-macos`, `arm64-windows`, or `x86_64-windows` below depending on your
architecure and OS, if necessary.

```shell
curl -LO https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-27/wasi-sdk-27.0-arm64-linux.tar.gz
tar xf wasi-sdk-27.0-arm64-linux.tar.gz
sudo mv wasi-sdk-27.0-arm64-linux /opt/wasi-sdk
export WASI_SDK_PATH=/opt/wasi-sdk
```

Finally, build and run `componentize-py`.

```shell
cargo run --release -- --help
```

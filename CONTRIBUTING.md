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

## Building from Source

### Prerequisites

- Tools needed to build [CPython](https://github.com/python/cpython) (Make, Clang, etc.)
- [Rust](https://rustup.rs/) stable 1.94 or later, including the `wasm32-wasip2` target

For Rust, something like this should work once you have `rustup`:

```shell
rustup update
rustup target add wasm32-wasip2
```

### Building and Running

First, make sure you've got all the submodules cloned.

```shell
git submodule update --init --recursive
```

Next, install WASI-SDK 33 to `/opt/wasi-sdk` (alternatively, you can specify a
different location and reference it later using the `WASI_SDK_PATH` environment
variable).  Replace `arm64-linux` with `x86_64-linux`, `arm64-macos`,
`x86_64-macos`, `arm64-windows`, or `x86_64-windows` below depending on your
architecure and OS, if necessary.

```shell
curl -LO https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-33/wasi-sdk-33.0-arm64-linux.tar.gz
tar xf wasi-sdk-33.0-arm64-linux.tar.gz
sudo mv wasi-sdk-33.0-arm64-linux /opt/wasi-sdk
export WASI_SDK_PATH=/opt/wasi-sdk
```

Finally, build and run `componentize-py`.

```shell
cargo run --release -- --help
```

## Publishing Releases

The release process currently requires several manual steps, unfortunately.
Automating this as part of CI would be great!

In the following, we'll pretend we're bumping the version from 0.22.1 to 0.23.0.
Remember to replace those numbers with the ones applicable to your release.

The first step is to update the version number in Cargo.toml, pyproject.toml,
and the examples.  We can use this bash one-liner:

```shell
for x in $(find examples/ -name README.md) Cargo.toml pyproject.toml; do sed -i 's/0\.22\.1/0.23.0/' $x; done
```

Note that that's a bit sketchy since it will match any `0.22.1` string, meaning
if we have a dependency with the same version number, it will get bumped also.
Be sure to run `git diff` and verify everything looks right before proceeding,
and make manual edits if necessary.

Next, commit your changes and open a PR.  Once that PR is merged, tag and sign
the commit using `git tag -s v0.23.0` and push it using `git push v0.23.0`.

Merging the PR to main will also kick off a release build, updating the `canary`
release.  When that finishes, go to the [canary release
page](https://github.com/bytecodealliance/componentize-py/releases/tag/canary)
and download the `componentize_py-0.23.0-*.whl` and
`componentize_py-0.23.0.tar.gz` files, move them into a newly-created `dist`
directory, and run the following:

```shell
python3 -m venv venv
source venv/bin/activate
pip install twine --upgrade
twine upload dist/*
```

You'll be prompted for an auth token.  If you don't have one and think you
should, please open an issue on this repository.

The above will publish Python wheels to pypi.org.  To publish to crates.io,
you'll need to do the following:

```shell
cargo login
bash stage.sh && (cd target/staged && cargo publish)
```

Again, you'll need an auth token; open an issue if you need one.

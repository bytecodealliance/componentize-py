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

- (optional) Tools needed to build [CPython](https://github.com/python/cpython)
  (Make, Clang, etc.)
- [Rust](https://rustup.rs/) stable 1.100 or later, including the
  `wasm32-wasip2` and `wasm32-wasip3` targets

For Rust, something like this should work once you have `rustup`:

```shell
rustup update
rustup target add wasm32-wasip2 wasm32-wasip3
```

### Building and Running Tests

To build the project and run the tests, use:

```shell
cargo test --release
```

By default, `build.rs` will download and use pre-built WASI-SDK and Cpython
binaries.  If you'd like to supply your own version of WASI-SDK and use it to
build CPython from source, install
[WASI-SDK](https://github.com/WebAssembly/wasi-sdk/releases) 34 or later, then
run:

```shell
rm -rf cpython
CPYTHON_BUILD_FROM_SOURCE=1 cargo test --release
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
making manual edits if necessary.

Next, commit your changes and open a PR.  Once that PR is merged, tag and sign
the commit using `git tag -s v0.23.0 -m v0.23.0` and push it using `git push
origin v0.23.0`.

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
bash stage.sh && (cd target/staged && cargo publish -p componentize-py-test-generator && cargo publish)
```

Again, you'll need an auth token; open an issue if you need one.

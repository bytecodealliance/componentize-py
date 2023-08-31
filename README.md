# componentize-py

**A [Bytecode Alliance](https://bytecodealliance.org/) project**

This is a tool to convert a Python application to a [WebAssembly
component](https://github.com/WebAssembly/component-model).  It takes the
following as input:

- a [WIT](https://github.com/WebAssembly/component-model/blob/main/design/mvp/WIT.md) file or directory
- the name of a [WIT world](https://github.com/WebAssembly/component-model/blob/main/design/mvp/WIT.md#wit-worlds) defined in the above file or directory
- the name of a Python module which targets said world
- a list of directories in which to find the Python module and its dependencies

The output is a component which may be run using
e.g. [`wasmtime`](https://github.com/bytecodealliance/wasmtime).  See the
[examples](https://github.com/dicej/componentize-py/tree/main/examples) and
[test](https://github.com/dicej/componentize-py/tree/main/src/test) directories
for examples.

For an example of running a sandboxed Python guest within a Python host, see
[component-sandbox-demo](https://github.com/dicej/component-sandbox-demo).

## Installing from PyPI

```shell
pip install componentize-py
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for details on how to contribute to the
project and build it from source.

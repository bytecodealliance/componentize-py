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
e.g. [`wasmtime`](https://github.com/bytecodealliance/wasmtime).

## Getting Started

First, install [Python 3.10 or later](https://www.python.org/) and
[pip](https://pypi.org/project/pip/) if you don't already have them.  Then,
install `componentize-py`:

```shell
pip install componentize-py
```

Next, create or download the WIT world you'd like to target, e.g.:

```shell
cat >hello.wit <<EOF
package example:hello
world hello {
  export hello: func() -> string
}
EOF
```

If you're using an IDE or just want to examine the bindings produced for the WIT
world, you can generate them using the `bindings` subcommand:

```shell
componentize-py -d hello.wit -w hello bindings .
```

Then, use the `hello` module produced by the command above to write your app:

```shell
cat >app.py <<EOF
import hello
class Hello(hello.Hello):
    def hello(self) -> str:
        return "Hello, World!"
EOF
```

And finally generate the component:

```shell
componentize-py -d hello.wit -w hello componentize app -o app.wasm
```

See the
[examples](https://github.com/bytecodealliance/componentize-py/tree/main/examples)
directories for more examples, including various ways to run the components you've
created.

For an example of running a sandboxed Python guest within a Python host, see
[component-sandbox-demo](https://github.com/dicej/component-sandbox-demo).

## Known Limitations

This project does not yet support [WIT
resources](https://github.com/WebAssembly/component-model/blob/main/design/mvp/WIT.md#item-resource)
or [interface
versions](https://github.com/WebAssembly/component-model/blob/main/design/mvp/WIT.md#package-declaration).
Both are coming soon.

See [the issue tracker](https://github.com/bytecodealliance/componentize-py/issues) for other known issues.

## Contributing

See
[CONTRIBUTING.md](https://github.com/bytecodealliance/componentize-py/tree/main/CONTRIBUTING.md)
for details on how to contribute to the project and build it from source.

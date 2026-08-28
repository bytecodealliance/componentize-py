# componentize-py

**A [Bytecode Alliance](https://bytecodealliance.org/) project**

This is a tool to convert a Python application to a [WebAssembly
component](https://github.com/WebAssembly/component-model). It takes the
following as input:

- a [WIT](https://github.com/WebAssembly/component-model/blob/main/design/mvp/WIT.md) file or directory
- the name of a [WIT world](https://github.com/WebAssembly/component-model/blob/main/design/mvp/WIT.md#wit-worlds) defined in the above file or directory
- the name of a Python module which targets said world
- a list of directories in which to find the Python module and its dependencies

The output is a component which may be run using
e.g. [`wasmtime`](https://github.com/bytecodealliance/wasmtime).

## Getting Started

First, install [Python 3.10 or later](https://www.python.org/) and
[pip](https://pypi.org/project/pip/) if you don't already have them. Then,
install `componentize-py`:

```shell
pip install componentize-py
```

Next, create or download the WIT world you'd like to target, e.g.:

```shell
cat >hello.wit <<EOF
package example:hello;
world hello {
  export hello: func() -> string;
}
EOF
```

If you're using an IDE or just want to examine the bindings produced for the WIT
world, you can generate them using the `bindings` subcommand:

```shell
componentize-py -d hello.wit -w hello bindings hello_guest
```

Then, use the bindings produced by the command above (a `wit` package inside
`hello_guest`) to write your app:

```shell
cat >app.py <<EOF
import wit

@wit.guest
class Hello(wit.WorldExports):
    def hello(self) -> str:
        return "Hello, World!"
EOF
```

And finally generate the component:

```shell
componentize-py -d hello.wit -w hello componentize --stub-wasi app -o app.wasm
```

To test it, you can install `wasmtime-py` and write a simple host app which uses
it to load and run our component:

```shell
pip install wasmtime==48.0.0
cat >host.py <<EOF
from wasmtime import Config, Engine, Store
from wasmtime.component import Component, Linker

config = Config()
config.cache = True
engine = Engine(config)
store = Store(engine)
component = Component.from_file(engine, "app.wasm")
linker = Linker(engine)
instance = linker.instantiate(store, component)
hello = instance.get_func(store, "hello")
print(f"component says: {hello(store)}")
EOF
```

And then run it:

```shell
 $ python3 host.py
component says: Hello, World!
```

See the
[examples](https://github.com/bytecodealliance/componentize-py/tree/main/examples)
directories for more examples, including various ways to run the components you've
created.

## Known Limitations

Currently, the application can only import dependencies during build time, which
means any imports used at runtime must be resolved at the top level of the
application module. For example, if `x` is a module with a submodule named `y`
the following may not work:

```python
import x

class Hello(hello.Hello):
    def hello(self) -> str:
        return x.y.foo()
```

That's because importing `x` does not necessarily resolve `y`. This can be
addressed by modifying the code to import `y` at the top level of the file:

```python
from x import y

class Hello(hello.Hello):
    def hello(self) -> str:
        return y.foo()
```

This limitation is being tracked as [issue
#23](https://github.com/bytecodealliance/componentize-py/issues/23).

See [the issue tracker](https://github.com/bytecodealliance/componentize-py/issues) for other known issues.

## Contributing

See
[CONTRIBUTING.md](https://github.com/bytecodealliance/componentize-py/tree/main/CONTRIBUTING.md)
for details on how to contribute to the project and build it from source.

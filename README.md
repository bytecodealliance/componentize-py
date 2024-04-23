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
componentize-py -d hello.wit -w hello componentize --stub-wasi app -o app.wasm
```

To test it, you can install `wasmtime-py` and use it to generate host-side
bindings for the component:

```shell
pip install wasmtime
python3 -m wasmtime.bindgen app.wasm --out-dir hello_host
```

Now we can write a simple host app using those bindings:

```shell
cat >host.py <<EOF
from hello_host import Root
from wasmtime import Config, Engine, Store

config = Config()
config.cache = True
engine = Engine(config)
store = Store(engine)
hello = Root(store)
print(f"component says: {hello.hello(store)}")
EOF
```

And finally run it:

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
application module.  For example, if `x` is a module with a submodule named `y`
the following may not work:

```python
import x

class Hello(hello.Hello):
    def hello(self) -> str:
        return x.y.foo()
```

That's because importing `x` does not necessarily resolve `y`.  This can be
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

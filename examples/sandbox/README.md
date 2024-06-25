# Example: `sandbox`

This is an example of how to use
[`wasmtime-py`](https://github.com/bytecodealliance/wasmtime-py) and
[`componentize-py`](https://github.com/bytecodealliance/componentize-py) to execute
sandboxed Python code snippets from within a Python app.

## Prerequisites

* `wasmtime-py` 18.0.0 or later
* `componentize-py` 0.13.5

```
pip install componentize-py==0.13.5 wasmtime==18.0.2
```

## Running the demo

```
componentize-py -d sandbox.wit componentize --stub-wasi guest -o sandbox.wasm
python3 -m wasmtime.bindgen sandbox.wasm --out-dir sandbox
python3 host.py "2 + 2"
```

## Examples

`host.py` accepts zero or more `exec` strings (e.g. newline-delimited
statements) followed by a final `eval` string (i.e. an expression).  Note that
any symbols you declare in an `exec` string must be explicitly added to the
global scope using `global`.  This ensures they are visible to subsequent `exec`
and `eval` strings.

```shell-session
 $ python3 host.py "2 + 2"
result: 4
 $ python3 host.py 'global foo
def foo(): return 42' 'foo()'
result: 42
```

### Time limit

`host.py` enforces a twenty second timeout on guest execution.  If and when the
timeout is reached, `wasmtime` will raise a `Trap` error.

```shell-session
 $ python3 host.py 'while True: pass' '1'
timeout!
Traceback (most recent call last):
  File "/Users/dicej/p/componentize-py/examples/sandbox/host.py", line 36, in <module>
    result = sandbox.exec(store, arg)
             ^^^^^^^^^^^^^^^^^^^^^^^^
...
```

### Memory limit

`host.py` limits guest memory usage to 20MB.  Any attempt to allocate beyond
that limit will fail.

```shell-session
 $ python3 host.py 'global foo
foo = bytes(100 * 1024 * 1024)' 'foo[42]'
exec error: MemoryError
```

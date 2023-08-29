import matrix_math
import sys

from matrix_math import Root, RootImports, imports
from matrix_math.types import Ok, Err, Result
from matrix_math.imports import environment
from wasmtime import Store
from typing import List, Tuple

class Host(imports.Host):
    def log(self, message: str) -> Result[None, str]:
        print(f"guest log: {message}")
        return Ok(None)

class HostEnvironment(environment.HostEnvironment):
    def get_environment(self) -> List[Tuple[str, str]]:
        return []

    def get_arguments(self) -> List[str]:
        return []

args = sys.argv[1:]
if len(args) != 2:
    print("usage: python3 host.py <matrix> <matrix>", file=sys.stderr)
    exit(-1)

store = Store()

matrix_math = Root(
    store,
    RootImports(
        host=Host(),
        # As of this writing, `wasmtime-py` does not yet support WASI Preview 2,
        # and our example won't use it at runtime anyway, so we provide `None`
        # for all `wasi-cli` interfaces:
        poll=None,
        monotonic_clock=None,
        wall_clock=None,
        streams=None,
        types=None,
        preopens=None,
        random=None,
        environment=HostEnvironment(),
        exit=None,
        stdin=None,
        stdout=None,
        stderr=None,
        terminal_input=None,
        terminal_output=None,        
        terminal_stdin=None,
        terminal_stdout=None,
        terminal_stderr=None
    )
)

result = matrix_math.multiply(store, eval(args[0]), eval(args[1]))

if isinstance(result, Ok):
    print(f"result: {result.value}")
else:
    print(f"eval error: {result.value}")


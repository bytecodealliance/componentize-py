# This app can either be used as a library (by calling `matrix-math#multiply`)
# or a CLI command (by calling `wasi:cli/run#run`)

import sys
import numpy
import wit_world
from wit_world import exports
from componentize_py_types import Err


class WitWorld(wit_world.WitWorld):
    def multiply(self, a: list[list[float]], b: list[list[float]]) -> list[list[float]]:
        print(f"matrix_multiply received arguments {a} and {b}")
        return numpy.matmul(a, b).tolist()  # type: ignore


class Run(exports.Run):
    def run(self) -> None:
        args = sys.argv[1:]
        if len(args) != 2:
            print("usage: matrix-math <matrix> <matrix>", file=sys.stderr)
            exit(-1)

        print(WitWorld().multiply(eval(args[0]), eval(args[1])))

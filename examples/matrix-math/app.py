# This app can either be used as a library (by calling `matrix-math#multiply`)
# or a CLI command (by calling `wasi:cli/run#run`)

import sys
import numpy
import matrix_math
from matrix_math import exports
from matrix_math.types import Err

class MatrixMath(matrix_math.MatrixMath):
    def multiply(self, a: list[list[float]], b: list[list[float]]) -> list[list[float]]:
        print(f"matrix_multiply received arguments {a} and {b}")
        return numpy.matmul(a, b).tolist()

class Run(exports.Run):
    def run(self):
        args = sys.argv[1:]
        if len(args) != 2:
            print(f"usage: matrix-math <matrix> <matrix>", file=sys.stderr)
            exit(-1)

        print(MatrixMath().multiply(eval(args[0]), eval(args[1])))


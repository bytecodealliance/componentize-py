import numpy
import matrix_math
from matrix_math.types import Err
from matrix_math import log
from typing import NoReturn

def handle(e: Exception) -> NoReturn:
    message = str(e)
    if message == '':
        raise Err(f"{type(e).__name__}")
    else:
        raise Err(f"{type(e).__name__}: {message}")

class MatrixMath(matrix_math.MatrixMath):
    def multiply(self, a: list[list[float]], b: list[list[float]]) -> list[list[float]]:
        try:
            log(f"matrix_multiply received arguments {a} and {b}")
            return numpy.matmul(a, b).tolist()
        except Exception as e:
            handle(e)

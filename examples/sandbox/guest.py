import wit_world
from componentize_py_types import Err
import json


def handle(e: Exception) -> Err[str]:
    message = str(e)
    if message == "":
        return Err(f"{type(e).__name__}")
    else:
        return Err(f"{type(e).__name__}: {message}")


class WitWorld(wit_world.WitWorld):
    def eval(self, expression: str) -> str:
        try:
            return json.dumps(eval(expression))
        except Exception as e:
            raise handle(e)

    def exec(self, statements: str) -> None:
        try:
            exec(statements)
        except Exception as e:
            raise handle(e)

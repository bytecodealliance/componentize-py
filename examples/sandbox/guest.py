import sandbox
from sandbox.types import Err
import json

def handle(e: Exception):
    message = str(e)
    if message == '':
        raise Err(f"{type(e).__name__}")
    else:
        raise Err(f"{type(e).__name__}: {message}")

class Sandbox(sandbox.Sandbox):
    def eval(self, expression: str) -> str:
        try:
            return json.dumps(eval(expression))
        except Exception as e:
            handle(e)

    def exec(self, statements: str):
        try:
            exec(statements)
        except Exception as e:
            handle(e)

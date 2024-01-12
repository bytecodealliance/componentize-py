from tests.exports import resource_floats_exports
from tests.imports.resource_floats_imports import Float as HostFloat
from typing import Self

class Float(resource_floats_exports.Float):
    def __init__(self, v: float):
        self.value = HostFloat(v + 1)

    def get(self) -> str:
        return self.value.get() + 3

    @staticmethod
    def add(a: Self, b: float) -> Self:
        return Float(HostFloat.add(a.value, b).get() + 5)
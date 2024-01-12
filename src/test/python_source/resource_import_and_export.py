from tests.exports import resource_import_and_export
from tests.imports.resource_import_and_export import Thing as HostThing
from typing import Self

class Thing(resource_import_and_export.Thing):
    def __init__(self, v: int):
        self.value = HostThing(v + 7)

    def foo(self) -> int:
        return self.value.foo() + 3

    def bar(self, v: int):
        self.value.bar(v + 4)

    @staticmethod
    def baz(a: Self, b: Self) -> Self:
        with HostThing.baz(a.value, b.value) as bar:
            value = bar.foo()
        return Thing(value + 9)
from tests.exports import resource_with_lists
from tests.imports.resource_with_lists import Thing as HostThing
from typing import List

class Thing(resource_with_lists.Thing):
    def __init__(self, v: bytes):
        x = bytearray(v)
        x.extend(b" Thing.__init__")
        self.value = HostThing(bytes(x))

    def foo(self) -> bytes:
        x = bytearray(self.value.foo())
        x.extend(b" Thing.foo")
        return bytes(x)

    def bar(self, v: bytes):
        x = bytearray(v)
        x.extend(b" Thing.bar")
        self.value.bar(bytes(x))

    @staticmethod
    def baz(v: bytes) -> bytes:
        x = bytearray(v)
        x.extend(b" Thing.baz")
        y = bytearray(HostThing.baz(bytes(x)))
        y.extend(b" Thing.baz again")
        return bytes(y)
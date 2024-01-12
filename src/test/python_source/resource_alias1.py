from tests.exports import resource_alias1
from tests.imports.resource_alias1 import Thing as HostThing

class Thing(resource_alias1.Thing):
    def __init__(self, v: str):
        self.value = HostThing(v + " Thing.__init__")

    def get(self) -> str:
        return self.value.get() + " Thing.get"

def wrap_thing(thing: HostThing) -> Thing:
    mine = Thing.__new__(Thing)
    mine.value = thing
    return mine
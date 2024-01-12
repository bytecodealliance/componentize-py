from tests.exports import resource_aggregates
from tests.imports.resource_aggregates import Thing as HostThing

class Thing(resource_aggregates.Thing):
    def __init__(self, v: int):
        self.value = HostThing(v + 1)
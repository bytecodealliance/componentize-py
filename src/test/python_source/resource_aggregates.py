from tests.exports.componentize_py.test import resource_aggregates
from tests.imports.componentize_py.test.resource_aggregates import Thing as HostThing

class Thing(resource_aggregates.Thing):
    def __init__(self, v: int):
        self.value = HostThing(v + 1)
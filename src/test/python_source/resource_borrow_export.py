from tests.exports import resource_borrow_export

class Thing(resource_borrow_export.Thing):
    def __init__(self, v: int):
        self.value = v + 1
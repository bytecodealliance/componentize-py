from tests.exports import streams_and_futures

class Thing(streams_and_futures.Thing):
    def __init__(self, v: str):
        self.value = v

    async def get(self) -> str:
        return self.value

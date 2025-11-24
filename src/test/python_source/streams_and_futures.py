import tests

from tests.exports import streams_and_futures

class Thing(streams_and_futures.Thing):
    def __init__(self, v: str):
        self.value = v

    async def get(self, delay_millis: int) -> str:
        if delay_millis > 0:
            await tests.sleep(delay_millis)

        return self.value

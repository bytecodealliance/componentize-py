import asyncio

from proxy.imports import types2 as types, streams2 as streams, poll2 as poll
from proxy.imports.streams2 import StreamStatus
from typing import Optional

READ_SIZE: int = 16 * 1024

class Stream:
    def __init__(self, body: int):
        self.pollable = streams.subscribe_to_input_stream(body)
        self.body = body
        self.saw_end = False

    async def next(self) -> Optional[bytes]:
        if self.saw_end:
            return None
        else:
            while True:
                buffer, status = streams.read(self.body, READ_SIZE)
                if status == StreamStatus.ENDED:
                    types.finish_incoming_stream(self.body)
                    self.saw_end = True

                if buffer:
                    return buffer
                elif status == StreamStatus.ENDED:
                    return None
                else:
                    await asyncio.get_event_loop().register(self.pollable)

class Sink:
    def __init__(self, body: int):
        self.pollable = streams.subscribe_to_output_stream(body)
        self.body = body

    async def send(self, chunk: bytes):
        offset = 0
        while True:
            count = streams.write(self.body, chunk[offset:])
            offset += count
            if offset == len(chunk):
                return
            else:
                await asyncio.get_event_loop().register(self.pollable)

    async def close(self):
        types.finish_outgoing_stream(self.body)
        
class PollLoop(asyncio.AbstractEventLoop):
    def __init__(self):
        self.wakers = []
        self.running = False
        self.handles = []
        self.exception = None

    async def register(self, pollable: int):
        waker = self.create_future()
        self.wakers.append((pollable, waker))
        await waker

    def get_debug(self):
        return False

    def run_until_complete(self, future):
        future = asyncio.ensure_future(future, loop=self)

        self.running = True
        asyncio.events._set_running_loop(self)
        while self.running and not future.done():
            handle = self.handles[0]
            self.handles = self.handles[1:]
            if not handle._cancelled:
                handle._run()
                
            if self.wakers:
                [pollables, wakers] = list(map(list, zip(*self.wakers)))
                
                new_wakers = []
                for (ready, pollable), waker in zip(zip(poll.poll_oneoff(pollables), pollables), wakers):
                    if ready:
                        waker.set_result(None)
                    else:
                        new_wakers.append((pollable, waker))

                self.wakers = new_wakers

            if self.exception is not None:
                raise self.exception
            
        future.result()

    def is_running(self):
        return self.running

    def is_closed(self):
        return not self.running

    def stop(self):
        self.running = False

    def close(self):
        self.running = False

    def shutdown_asyncgens(self):
        pass

    def call_exception_handler(self, context):
        self.exception = context.get('exception', None)

    def call_soon(self, callback, *args, context=None):
        handle = asyncio.Handle(callback, args, self, context)
        self.handles.append(handle)
        return handle

    def create_task(self, coroutine):
        return asyncio.Task(coroutine, loop=self)

    def create_future(self):
        return asyncio.Future(loop=self)

from typing import Optional

from proxy.types import Err
from proxy.imports import isyswasfa_io_poll
from proxy.imports.streams import InputStream, OutputStream, StreamError_Closed

# Maximum number of bytes to read at a time
_READ_SIZE: int = 64 * 1024

class Stream:
    """Reader abstraction over `wasi:io/streams#input-stream`."""
    def __init__(self, stream: InputStream):
        self.stream: Optional[InputStream] = stream

    async def next(self) -> Optional[bytes]:
        """Wait for the next chunk of data to arrive on the stream.

        This will return `None` when the end of the stream has been reached.
        """
        while True:
            try:
                if self.stream is None:
                    return None
                else:
                    buffer = self.stream.read(_READ_SIZE)
                    if len(buffer) == 0:
                        with self.stream.subscribe() as pollable:
                            await isyswasfa_io_poll.block(pollable)
                    else:
                        return buffer
            except Err as e:
                if isinstance(e.value, StreamError_Closed):
                    self.__exit__()
                else:
                    raise e
        
    def __enter__(self):
        return self

    def __exit__(self, *args):
        """Close the stream, indicating no further data will be read."""
        if self.stream is not None:
            self.stream.__exit__()
            self.stream = None

class Sink:
    """Writer abstraction over `wasi:io/streams#output-stream`."""
    def __init__(self, stream: OutputStream):
        self.stream = stream

    async def send(self, chunk: bytes):
        """Write the specified bytes to the sink.

        This may need to yield according to the backpressure requirements of the sink.
        """
        offset = 0
        flushing = False
        while True:
            count = self.stream.check_write()
            if count == 0:
                with self.stream.subscribe() as pollable:
                    await isyswasfa_io_poll.block(pollable)
            elif offset == len(chunk):
                if flushing:
                    return
                else:
                    self.stream.flush()
                    flushing = True
            else:
                count = min(count, len(chunk) - offset)
                self.stream.write(chunk[offset:offset+count])
                offset += count

    def __enter__(self):
        return self

    def __exit__(self, *args):
        """Close the sink, indicating no further data will be written."""
        self.stream.__exit__()

        

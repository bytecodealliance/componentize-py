"""Defines a custom `asyncio` event loop backed by WASI's `poll_oneoff`.

As of WASI Preview 2, there is not yet a standard for first-class, composable
asynchronous functions and streams.  We expect that little or none of this
boilerplate will be needed once those features arrive in Preview 3.
"""

import asyncio
import socket
import subprocess

from proxy.imports import types2 as types, streams2 as streams, poll2 as poll
from proxy.imports.streams2 import StreamStatus
from typing import Optional, cast

# Maximum number of bytes to read at a time
READ_SIZE: int = 16 * 1024

class Stream:
    """Reader abstraction over `wasi-cli`'s low-level stream pseudo-resource."""
    def __init__(self, stream: int):
        self.pollable = streams.subscribe_to_input_stream(stream)
        self.stream = stream
        self.saw_end = False

    async def next(self) -> Optional[bytes]:
        """Wait for the next chunk of data to arrive on the stream.

        This will return `None` when the end of the stream has been reached.
        """
        if self.saw_end:
            return None
        else:
            while True:
                buffer, status = streams.read(self.stream, READ_SIZE)
                if status == StreamStatus.ENDED:
                    types.finish_incoming_stream(self.stream)
                    self.saw_end = True

                if buffer:
                    return buffer
                elif status == StreamStatus.ENDED:
                    return None
                else:
                    await register(cast(PollLoop, asyncio.get_event_loop()), self.pollable)

class Sink:
    """Writer abstraction over `wasi-cli`'s low-level stream pseudo-resource."""
    def __init__(self, stream: int):
        self.pollable = streams.subscribe_to_output_stream(stream)
        self.stream = stream

    async def send(self, chunk: bytes):
        """Write the specified bytes to the stream.

        This may need to yield according to the backpressure requirements of the stream.
        """
        offset = 0
        while True:
            count = streams.write(self.stream, chunk[offset:])
            offset += count
            if offset == len(chunk):
                return
            else:
                await register(cast(PollLoop, asyncio.get_event_loop()), self.pollable)

    def close(self):
        """Close the stream, indicating no further data will be written."""
        
        types.finish_outgoing_stream(self.stream)
        
class PollLoop(asyncio.AbstractEventLoop):
    """Custom `asyncio` event loop backed by WASI's `poll_oneoff` function."""
    
    def __init__(self):
        self.wakers = []
        self.running = False
        self.handles = []
        self.exception = None

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

    # The remaining methods should be irrelevant for our purposes and thus unimplemented

    def run_forever(self):
        raise NotImplementedError

    async def shutdown_default_executor(self):
        raise NotImplementedError

    def _timer_handle_cancelled(self, handle):
        raise NotImplementedError

    def call_later(self, delay, callback, *args, context=None):
        raise NotImplementedError

    def call_at(self, when, callback, *args, context=None):
        raise NotImplementedError

    def time(self):
        raise NotImplementedError

    def call_soon_threadsafe(self, callback, *args, context=None):
        raise NotImplementedError

    def run_in_executor(self, executor, func, *args):
        raise NotImplementedError

    def set_default_executor(self, executor):
        raise NotImplementedError

    async def getaddrinfo(self, host, port, *,
                          family=0, type=0, proto=0, flags=0):
        raise NotImplementedError

    async def getnameinfo(self, sockaddr, flags=0):
        raise NotImplementedError

    async def create_connection(
            self, protocol_factory, host=None, port=None,
            *, ssl=None, family=0, proto=0,
            flags=0, sock=None, local_addr=None,
            server_hostname=None,
            ssl_handshake_timeout=None,
            ssl_shutdown_timeout=None,
            happy_eyeballs_delay=None, interleave=None):
        raise NotImplementedError

    async def create_server(
            self, protocol_factory, host=None, port=None,
            *, family=socket.AF_UNSPEC,
            flags=socket.AI_PASSIVE, sock=None, backlog=100,
            ssl=None, reuse_address=None, reuse_port=None,
            ssl_handshake_timeout=None,
            ssl_shutdown_timeout=None,
            start_serving=True):
        raise NotImplementedError

    async def sendfile(self, transport, file, offset=0, count=None,
                       *, fallback=True):
        raise NotImplementedError

    async def start_tls(self, transport, protocol, sslcontext, *,
                        server_side=False,
                        server_hostname=None,
                        ssl_handshake_timeout=None,
                        ssl_shutdown_timeout=None):
        raise NotImplementedError

    async def create_unix_connection(
            self, protocol_factory, path=None, *,
            ssl=None, sock=None,
            server_hostname=None,
            ssl_handshake_timeout=None,
            ssl_shutdown_timeout=None):
        raise NotImplementedError

    async def create_unix_server(
            self, protocol_factory, path=None, *,
            sock=None, backlog=100, ssl=None,
            ssl_handshake_timeout=None,
            ssl_shutdown_timeout=None,
            start_serving=True):
        raise NotImplementedError

    async def connect_accepted_socket(
            self, protocol_factory, sock,
            *, ssl=None,
            ssl_handshake_timeout=None,
            ssl_shutdown_timeout=None):
        raise NotImplementedError

    async def create_datagram_endpoint(self, protocol_factory,
                                       local_addr=None, remote_addr=None, *,
                                       family=0, proto=0, flags=0,
                                       reuse_address=None, reuse_port=None,
                                       allow_broadcast=None, sock=None):
        raise NotImplementedError

    async def connect_read_pipe(self, protocol_factory, pipe):
        raise NotImplementedError

    async def connect_write_pipe(self, protocol_factory, pipe):
        raise NotImplementedError

    async def subprocess_shell(self, protocol_factory, cmd, *,
                               stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE,
                               **kwargs):
        raise NotImplementedError

    async def subprocess_exec(self, protocol_factory, *args,
                              stdin=subprocess.PIPE,
                              stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE,
                              **kwargs):
        raise NotImplementedError

    def add_reader(self, fd, callback, *args):
        raise NotImplementedError

    def remove_reader(self, fd):
        raise NotImplementedError

    def add_writer(self, fd, callback, *args):
        raise NotImplementedError

    def remove_writer(self, fd):
        raise NotImplementedError

    async def sock_recv(self, sock, nbytes):
        raise NotImplementedError

    async def sock_recv_into(self, sock, buf):
        raise NotImplementedError

    async def sock_recvfrom(self, sock, bufsize):
        raise NotImplementedError

    async def sock_recvfrom_into(self, sock, buf, nbytes=0):
        raise NotImplementedError

    async def sock_sendall(self, sock, data):
        raise NotImplementedError

    async def sock_sendto(self, sock, data, address):
        raise NotImplementedError

    async def sock_connect(self, sock, address):
        raise NotImplementedError

    async def sock_accept(self, sock):
        raise NotImplementedError

    async def sock_sendfile(self, sock, file, offset=0, count=None,
                            *, fallback=None):
        raise NotImplementedError

    def add_signal_handler(self, sig, callback, *args):
        raise NotImplementedError

    def remove_signal_handler(self, sig):
        raise NotImplementedError

    def set_task_factory(self, factory):
        raise NotImplementedError

    def get_task_factory(self):
        raise NotImplementedError

    def get_exception_handler(self):
        raise NotImplementedError

    def set_exception_handler(self, handler):
        raise NotImplementedError

    def default_exception_handler(self, context):
        raise NotImplementedError

    def set_debug(self, enabled):
        raise NotImplementedError

async def register(loop: PollLoop, pollable: int):
    waker = loop.create_future()
    loop.wakers.append((pollable, waker))
    await waker

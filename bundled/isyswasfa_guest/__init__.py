import asyncio
import socket
import subprocess
from dataclasses import dataclass
from typing import Any, Union, Optional
from contextvars import ContextVar, Context

from proxy.types import Result, Ok, Err
from proxy.imports import isyswasfa
from proxy.imports.isyswasfa import (
    PollInput, PollInput_Ready, PollInputReady, PollInput_Listening, PollOutput, PollOutput_Listen,
    PollOutputListen, PollOutput_Pending, PollOutputPending, PollOutput_Ready, PollOutputReady, Ready, Pending,
    Cancel
)

_future_state: ContextVar[int] = ContextVar("_future_state")

class _Loop(asyncio.AbstractEventLoop):
    def __init__(self):
        self.running: bool = False
        self.handles: dict[int, list[asyncio.Handle]] = {}
        self.exception: Optional[Any] = None

    def poll(self, future_state: int):
        while True:
            handles = self.handles.pop(future_state, [])
            for handle in handles:
                if not handle._cancelled:
                    handle._run()
                
            if self.exception is not None:
                raise self.exception

            if len(handles) == 0:
                return
    
    def get_debug(self):
        return False

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
        global _pollables

        future_state = context[_future_state]
        handle = asyncio.Handle(callback, args, self, context)
        if self.handles.get(future_state) is None:
            self.handles[future_state] = []
        self.handles[future_state].append(handle)
        _pollables.add(future_state)
        return handle

    def create_task(self, coroutine, context=None):
        return asyncio.Task(coroutine, loop=self, context=context) # type: ignore

    def create_future(self):
        return asyncio.Future(loop=self)

    # The remaining methods should be irrelevant for our purposes and thus unimplemented

    def run_until_complete(self, future):
        raise NotImplementedError

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

@dataclass
class _FutureStatePending:
    ready: Ready
    future: Any

@dataclass
class _FutureStateReady:
    result: Any

_FutureState = Union[_FutureStatePending, _FutureStateReady]

@dataclass
class _ListenState:
    future: Any
    future_state: int
    cancel: Optional[Cancel]
      
@dataclass
class _PendingState:
    pending: Pending
    future: Any

_loop = _Loop()
asyncio.set_event_loop(_loop)
_loop.running = True
asyncio.events._set_running_loop(_loop)

_pending: list[_PendingState] = []
_poll_output: list[PollOutput] = []
_listen_states: dict[int, _ListenState] = {}
_next_listen_state: int = 0
_future_states: dict[int, _FutureState] = {}
_next_future_state: int = 0
_pollables: set[int] = set()

def _set_future_state(future_state: int):
    _future_state.set(future_state)

def _poll_future(future: Any):
    raise NotImplementedError

def _push_listens(future_state: int):
    global _pending
    global _poll_output
    global _next_listen_state
    global _listen_states
    
    pending = _pending
    _pending = []
    for p in pending:
        # todo: wrap around at 2^32 and then skip any used slots        
        listen_state = _next_listen_state
        _next_listen_state += 1
        _listen_states[listen_state] = _ListenState(p.future, future_state, None)
        
        _poll_output.append(PollOutput_Listen(PollOutputListen(listen_state, p.pending)))

def first_poll(coroutine: Any) -> Result[Any, Any]:
    return _first_poll(coroutine, True)
    
def _first_poll(coroutine: Any, poll: bool) -> Result[Any, Any]:
    global _loop
    global _pending
    global _next_future_state
    global _future_states
    global _poll_output
    
    # todo: wrap around at 2^32 and then skip any used slots
    future_state = _next_future_state
    _next_future_state += 1
    _future_states[future_state] = _FutureStateReady(None)

    context = Context()
    context.run(_set_future_state, future_state)
    future = asyncio.create_task(coroutine, context=context)
    
    if poll:
        _loop.poll(future_state)

    if future.done():
        _pending.clear()
        _future_states.pop(future_state)
        _pollables.remove(future_state)
        try:
            return Ok(future.result())
        except Err as e:
            return e
    else:
        pending, cancel, ready = isyswasfa.make_task()

        _future_states[future_state] = _FutureStatePending(ready, future)
        
        _push_listens(future_state)
        _poll_output.append(PollOutput_Pending(PollOutputPending(future_state, cancel)))
        
        raise Err(pending)

def get_ready(ready: Ready) -> Any:
    global _future_states
    
    with ready as ready:
        value = _future_states.pop(ready.state())
        assert isinstance(value, _FutureStateReady)
        return value.result
    
async def await_ready(pending: Pending) -> Ready:
    global _loop
    global _pending

    future = _loop.create_future()
    _pending.append(_PendingState(pending, future))
    return await future

def poll(input: list[PollInput]) -> list[PollOutput]:
    global _loop
    global _pending
    global _pollables
    global _poll_output
    global _listen_states
    global _future_states

    for i in input:
        if isinstance(i, PollInput_Ready):
            value = i.value
            listen_state = _listen_states.pop(value.state)

            if listen_state.future is not None:
                listen_state.future.set_result(value.ready)
                listen_state.future = None

            if listen_state.cancel is not None:
                listen_state.cancel.__exit__()
        elif isinstance(i, PollInput_Listening):
            _listen_states[i.value.state].cancel = i.value.cancel
        else:
            raise NotImplementedError("todo: handle cancellation")
                
    while True:
        pollables = _pollables
        _pollables = set()

        if pollables:
            for future_state in pollables:
                state = _future_states[future_state]
                if isinstance(state, _FutureStatePending):
                    _loop.poll(future_state)
                
                    if state.future.done():
                        _pending.clear()

                        _future_states[future_state] = _FutureStateReady(state.future.result())

                        _poll_output.append(PollOutput_Ready(PollOutputReady(future_state, state.ready)))
                    else:
                        _push_listens(future_state)
        else:
            poll_output = _poll_output
            _poll_output = []
            return poll_output

def spawn(coroutine: Any):
    global _pending
    
    pending = _pending
    _pending = []
    
    try:
        _first_poll(coroutine, False)
    except Err as e:
        e.value.__exit__()
        
    _pending = pending

import componentize_py_runtime
import componentize_py_async_support
import weakref

from typing import TypeVar, Generic, cast, Self, Any, Callable
from types import TracebackType
from componentize_py_async_support import _ReturnCode

T = TypeVar('T')

class FutureReader(Generic[T]):
    """Represents the readable end of a Component Model `future`.

    Each object of this type should be closed promptly using either context
    management (e.g. a `with` statement) or by calling `read` in order to notify
    the owner of the writable end of the `future` that the readable end has been
    closed.  If the object becomes unreachable without being closed, it will be
    closed via finalization.

    """
    def __init__(self, type_: int, handle: int):
        """Constructor for internal use by generated code.

        Application code should not call this directly.  Instead,
        `componentize-py` will generate a constructor function for each unique
        `future` type used by the target world.  Each such function will return
        a (`FutureReader`, `FutureWriter`) pair and use this constructor behind
        the scenes.

        """
        self.type_ = type_
        self.handle: int | None = handle
        self.finalizer = weakref.finalize(self, componentize_py_runtime.future_drop_readable, type_, handle)

    async def read(self) -> T:
        """Asynchronously read the value sent to this `future`.

        The awaitable returned by this function will resolve when a value has
        been delivered to the `future`.

        Calling this function consumes the target object; any attempt to call
        `read` more than once will raise an `AssertionError`.

        """
        
        self.finalizer.detach()
        handle = self.handle
        self.handle = None
        if handle is not None:
            result = await componentize_py_async_support.await_result(
                componentize_py_runtime.future_read(self.type_, handle)
            )
            componentize_py_runtime.future_drop_readable(self.type_, handle)
            return cast(T, result)
        else:
            raise AssertionError

    def __enter__(self) -> Self:
        return self
        
    def __exit__(self,
                 exc_type: type[BaseException] | None,
                 exc_value: BaseException | None,
                 traceback: TracebackType | None) -> bool | None:
        if self.handle is not None:
            self.finalizer.detach()
            handle = self.handle
            self.handle = None
            componentize_py_runtime.future_drop_readable(self.type_, handle)

        return None

async def _write(type_: int, handle: int, value: Any) -> None:
    await componentize_py_async_support.await_result(
        componentize_py_runtime.future_write(type_, handle, value)
    )
    componentize_py_runtime.future_drop_writable(type_, handle)    

def _write_default(type_: int, handle: int, default: Callable[[], Any]) -> None:
    componentize_py_async_support.spawn(_write(type_, handle, default()))
            
class FutureWriter(Generic[T]):
    """Represents the writable end of a Component Model `future`.

    Each object of this type should be closed promptly using either context
    management (e.g. a `with` statement) or by calling `write` in order to send
    a value to the readable end of the `future.  If the object becomes
    unreachable without being closed, it will be closed via finalization after
    sending the default value which was specified during construction.

    """
    def __init__(self, type_: int, handle: int, default: Callable[[], T]):
        """Constructor for internal use by generated code.

        Application code should not call this directly.  Instead,
        `componentize-py` will generate a constructor function for each unique
        `future` type used by the target world.  Each such function will return
        a (`FutureReader`, `FutureWriter`) pair and use this constructor behind
        the scenes.

        """
        self.type_ = type_
        self.handle: int | None = handle
        self.default = default
        self.finalizer = weakref.finalize(self, _write_default, type_, handle, default)

    async def write(self, value: T) -> bool:
        """Asynchronously write a value to the `future`.

        The awaitable returned by this function will resolve once either the
        value has been delivered to the readable end of the `future` or the
        readable end has been closed.

        Calling this function consumes the target object; any attempt to call
        `write` more than once will raise an `AssertionError`.

        """
        self.finalizer.detach()
        handle = self.handle
        self.handle = None
        if handle is not None:
            code, _ = await componentize_py_async_support.await_result(
                componentize_py_runtime.future_write(self.type_, handle, value)
            )
            componentize_py_runtime.future_drop_writable(self.type_, handle)
            match code:
                case _ReturnCode.COMPLETED:
                    return True
                case _ReturnCode.DROPPED:
                    return False
                case _ReturnCode.CANCELLED:
                    # todo
                    raise NotImplementedError
                case _:
                    raise AssertionError
        else:
            raise AssertionError

    def __enter__(self) -> Self:
        return self
        
    def __exit__(self,
                 exc_type: type[BaseException] | None,
                 exc_value: BaseException | None,
                 traceback: TracebackType | None) -> bool | None:
        if self.handle is not None:
            componentize_py_async_support.spawn(self.write(self.default()))

        return None

import componentize_py_runtime
import componentize_py_async_support
import weakref

from typing import TypeVar, Generic, Self, cast
from types import TracebackType
from componentize_py_async_support import _ReturnCode

class ByteStreamReader:
    """Represents the readable end of a Component Model `stream<u8>`.

    Each object of this type should be closed promptly using context management
    (e.g. a `with` statement) in order to notify the owner of the writable end
    of the `stream` that the readable end has been closed.  If the object
    becomes unreachable without being closed, it will be closed via
    finalization.

    """
    def __init__(self, type_: int, handle: int):
        """Constructor for internal use by generated code.

        Application code should not call this directly.  Instead,
        `componentize-py` will generate `byte_stream` function if the target
        world uses the `stream<u8>` type.  That function will return a
        (`ByteStreamReader`, `ByteStreamWriter`) pair and use this constructor
        behind the scenes.

        """
        self.writer_dropped = False
        self.type_ = type_
        self.handle: int | None = handle
        self.finalizer = weakref.finalize(self, componentize_py_runtime.stream_drop_readable, type_, handle)

    async def read(self, max_count: int) -> bytes:
        """Asynchronously read up to `max_count` bytes sent to this `stream`.

        The awaitable returned by this function will resolve when either at
        least one byte has been delivered to the `stream` or the writable end
        has been closed (in which case the `writer_dropped` field will be set to
        `True`).

        Only one `read` operation is allowed at a time for a given object.  Any
        attempt to start a second read while the first is still in progress will
        raise an `AssertionError`.

        """
        if self.writer_dropped:
            return bytes()
        
        handle = self.handle
        self.handle = None
        code, values = await self._read(max_count, handle)
        self.handle = handle

        if code == _ReturnCode.DROPPED:
            self.writer_dropped = True
        
        return values
        
    async def _read(self, max_count: int, handle: int | None) -> tuple[int, bytes]:
        if handle is not None:
            return cast(tuple[int, bytes], await componentize_py_async_support.await_result(
                componentize_py_runtime.stream_read(self.type_, handle, max_count)
            ))
        else:
            raise AssertionError

    def __enter__(self) -> Self:
        return self
        
    def __exit__(self,
                 exc_type: type[BaseException] | None,
                 exc_value: BaseException | None,
                 traceback: TracebackType | None) -> bool | None:
        self.finalizer.detach()
        handle = self.handle
        self.handle = None
        if handle is not None:
            componentize_py_runtime.stream_drop_readable(self.type_, handle)
        return None

class ByteStreamWriter:
    """Represents the writable end of a Component Model `stream<u8>`.

    Each object of this type should be closed promptly using context management
    (e.g. a `with` statement) in order to notify the owner of the readable end
    of the `stream` that the writable end has been closed.  If the object
    becomes unreachable without being closed, it will be closed via
    finalization.

    """
    def __init__(self, type_: int, handle: int):
        """Constructor for internal use by generated code.

        Application code should not call this directly.  Instead,
        `componentize-py` will generate `byte_stream` function if the target
        world uses the `stream<u8>` type.  That function will return a
        (`ByteStreamReader`, `ByteStreamWriter`) pair and use this constructor
        behind the scenes.

        """
        self.reader_dropped = False
        self.type_ = type_
        self.handle: int | None = handle
        self.finalizer = weakref.finalize(self, componentize_py_runtime.stream_drop_writable, type_, handle)

    async def write(self, source: bytes) -> int:
        """Asynchronously write (some of) the specified bytes to the `stream`.

        The awaitable returned by this function will resolve when either at
        least one byte has been delivered to the `stream` or the readable end
        has been closed (in which case the `reader_dropped` field will be set to
        `True`).

        The return value is the total number of bytes delivered (which may be
        less than `len(source)`).  See also `write_all`, which attempts to write
        the entire buffer before returning.

        Only one `write` operation is allowed at a time for a given object.  Any
        attempt to start a second write while the first is still in progress
        will raise an `AssertionError`.

        """
        if self.reader_dropped:
            return 0
        
        handle = self.handle
        self.handle = None
        code, count = await self._write(source, handle)
        self.handle = handle

        if code == _ReturnCode.DROPPED:
            self.reader_dropped = True
        
        return count

    async def _write(self, source: bytes, handle: int | None) -> tuple[int, int]:
        if handle is not None:
            return await componentize_py_async_support.await_result(
                componentize_py_runtime.stream_write(self.type_, handle, source)
            )
        else:
            raise AssertionError

    async def write_all(self, source: bytes) -> int:
        """Asynchronously write the specified bytes to the `stream`.

        This calls `write` in a loop until either the entire buffer has been
        delivered or the readable end of the `stream` has been closed.  The
        return value is the total number of bytes delivered (which may be less
        than `len(source)`).

        """
        total = 0
        
        while len(source) > 0 and not self.reader_dropped:
            count = await self.write(source)
            source = source[count:]
            total += count
            
        return total

    def __enter__(self) -> Self:
        return self
        
    def __exit__(self,
                 exc_type: type[BaseException] | None,
                 exc_value: BaseException | None,
                 traceback: TracebackType | None) -> bool | None:
        self.finalizer.detach()
        handle = self.handle
        self.handle = None
        if handle is not None:
            componentize_py_runtime.stream_drop_writable(self.type_, handle)
        return None

T = TypeVar('T')

class StreamReader(Generic[T]):
    """Represents the readable end of a Component Model `stream`.

    Each object of this type should be closed promptly using context management
    (e.g. a `with` statement) in order to notify the owner of the writable end
    of the `stream` that the readable end has been closed.  If the object
    becomes unreachable without being closed, it will be closed via
    finalization.

    """
    def __init__(self, type_: int, handle: int):
        """Constructor for internal use by generated code.

        Application code should not call this directly.  Instead,
        `componentize-py` will generate a constructor function for each unique
        `stream` type used by the target world.  Each such function will return
        a (`StreamReader`, `StreamWriter`) pair and use this constructor behind
        the scenes.

        """
        self.writer_dropped = False
        self.type_ = type_
        self.handle: int | None = handle
        self.finalizer = weakref.finalize(self, componentize_py_runtime.stream_drop_readable, type_, handle)

    async def read(self, max_count: int) -> list[T]:
        """Asynchronously read up to `max_count` items sent to this `stream`.

        The awaitable returned by this function will resolve when either at
        least one item has been delivered to the `stream` or the writable end
        has been closed (in which case the `writer_dropped` field will be set to
        `True`).

        Only one `read` operation is allowed at a time for a given object.  Any
        attempt to start a second read while the first is still in progress will
        raise an `AssertionError`.

        """
        if self.writer_dropped:
            return []
        
        handle = self.handle
        self.handle = None
        code, values = await self._read(max_count, handle)
        self.handle = handle

        if code == _ReturnCode.DROPPED:
            self.writer_dropped = True
        
        return values
        
    async def _read(self, max_count: int, handle: int | None) -> tuple[int, list[T]]:
        if handle is not None:
            return cast(tuple[int, list[T]], await componentize_py_async_support.await_result(
                componentize_py_runtime.stream_read(self.type_, handle, max_count)
            ))
        else:
            raise AssertionError        

    def __enter__(self) -> Self:
        return self
        
    def __exit__(self,
                 exc_type: type[BaseException] | None,
                 exc_value: BaseException | None,
                 traceback: TracebackType | None) -> bool | None:
        self.finalizer.detach()
        handle = self.handle
        self.handle = None
        if handle is not None:
            componentize_py_runtime.stream_drop_readable(self.type_, handle)
        return None

class StreamWriter(Generic[T]):
    """Represents the writable end of a Component Model `stream`.

    Each object of this type should be closed promptly using context management
    (e.g. a `with` statement) in order to notify the owner of the readable end
    of the `stream` that the writable end has been closed.  If the object
    becomes unreachable without being closed, it will be closed via
    finalization.

    """
    def __init__(self, type_: int, handle: int):
        """Constructor for internal use by generated code.

        Application code should not call this directly.  Instead,
        `componentize-py` will generate a constructor function for each unique
        `stream` type used by the target world.  Each such function will return
        a (`StreamReader`, `StreamWriter`) pair and use this constructor behind
        the scenes.

        """
        self.reader_dropped = False
        self.type_ = type_
        self.handle: int | None = handle
        self.finalizer = weakref.finalize(self, componentize_py_runtime.stream_drop_writable, type_, handle)

    async def write(self, source: list[T]) -> int:
        """Asynchronously write (some of) the specified itmes to the `stream`.

        The awaitable returned by this function will resolve when either at
        least one item has been delivered to the `stream` or the readable end
        has been closed (in which case the `reader_dropped` field will be set to
        `True`).

        The return value is the total number of items delivered (which may be
        less than `len(source)`).  See also `write_all`, which attempts to write
        the entire buffer before returning.

        Only one `write` operation is allowed at a time for a given object.  Any
        attempt to start a second write while the first is still in progress will
        raise an `AssertionError`.

        """
        if self.reader_dropped:
            return 0
        
        handle = self.handle
        self.handle = None
        code, count = await self._write(source, handle)
        self.handle = handle

        if code == _ReturnCode.DROPPED:
            self.reader_dropped = True
        
        return count

    async def _write(self, source: list[T], handle: int | None) -> tuple[int, int]:
        if handle is not None:
            return await componentize_py_async_support.await_result(
                componentize_py_runtime.stream_write(self.type_, handle, source)
            )
        else:
            raise AssertionError        

    async def write_all(self, source: list[T]) -> int:
        """Asynchronously write the specified items to the `stream`.

        This calls `write` in a loop until either the entire buffer has been
        delivered or the readable end of the `stream` has been closed.  The
        return value is the total number of items delivered (which may be less
        than `len(source)`).

        """
        total = 0
        
        while len(source) > 0 and not self.reader_dropped:
            count = await self.write(source)
            source = source[count:]
            total += count
            
        return total

    def __enter__(self) -> Self:
        return self
        
    def __exit__(self,
                 exc_type: type[BaseException] | None,
                 exc_value: BaseException | None,
                 traceback: TracebackType | None) -> bool | None:
        self.finalizer.detach()
        handle = self.handle
        self.handle = None
        if handle is not None:
            componentize_py_runtime.stream_drop_writable(self.type_, handle)
        return None

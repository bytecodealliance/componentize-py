import traceback
import tests
import resource_borrow_export
import resource_aggregates
import resource_alias1
import resource_borrow_in_record
import componentize_py_async_support
import streams_and_futures as my_streams_and_futures

from componentize_py_types import Result, Ok, Err
from tests import exports, imports
from tests.imports import resource_borrow_import
from tests.imports import simple_import_and_export
from tests.imports import simple_async_import_and_export
from tests.imports import host_thing_interface
from tests.exports import resource_alias2
from tests.exports import streams_and_futures
from typing import Tuple, List, Optional
from foo_sdk.wit import exports as foo_exports
from foo_sdk.wit.imports.foo_interface import test as foo_test
from bar_sdk.wit import exports as bar_exports
from bar_sdk.wit.imports.foo_interface import test as bar_test

class SimpleExport(exports.SimpleExport):
    def foo(self, v: int) -> int:
        return v + 3

class SimpleImportAndExport(exports.SimpleImportAndExport):
    def foo(self, v: int) -> int:
        return simple_import_and_export.foo(v) + 3

class SimpleAsyncExport(exports.SimpleAsyncExport):
    async def foo(self, v: int) -> int:
        return v + 3

class SimpleAsyncImportAndExport(exports.SimpleAsyncImportAndExport):
    async def foo(self, v: int) -> int:
        return (await simple_async_import_and_export.foo(v)) + 3

class ResourceImportAndExport(exports.ResourceImportAndExport):
    pass

class ResourceBorrowExport(exports.ResourceBorrowExport):
    def foo(self, v: resource_borrow_export.Thing) -> int:
        return v.value + 2

class ResourceWithLists(exports.ResourceWithLists):
    pass

class ResourceAggregates(exports.ResourceAggregates):
    def foo(
        self,
        r1: exports.resource_aggregates.R1,
        r2: exports.resource_aggregates.R2,
        r3: exports.resource_aggregates.R3,
        t1: Tuple[resource_aggregates.Thing, exports.resource_aggregates.R1],
        t2: Tuple[resource_aggregates.Thing],
        v1: exports.resource_aggregates.V1,
        v2: exports.resource_aggregates.V2,
        l1: List[resource_aggregates.Thing],
        l2: List[resource_aggregates.Thing],
        o1: Optional[resource_aggregates.Thing],
        o2: Optional[resource_aggregates.Thing],
        result1: Result[resource_aggregates.Thing, None],
        result2: Result[resource_aggregates.Thing, None]
    ) -> int:
        if o1 is None:
            host_o1 = None
        else:
            host_o1 = o1.value
        
        if o2 is None:
            host_o2 = None
        else:
            host_o2 = o2.value

        if isinstance(result1, Ok):
            host_result1 = Ok(result1.value.value)
        else:
            host_result1 = result1
        
        if isinstance(result2, Ok):
            host_result2 = Ok(result2.value.value)
        else:
            host_result2 = result2

        return imports.resource_aggregates.foo(
            imports.resource_aggregates.R1(r1.thing.value),
            imports.resource_aggregates.R2(r2.thing.value),
            imports.resource_aggregates.R3(r3.thing1.value, r3.thing2.value),
            (t1[0].value, imports.resource_aggregates.R1(t1[1].thing.value)),
            (t2[0].value,),
            imports.resource_aggregates.V1_Thing(v1.value.value),
            imports.resource_aggregates.V2_Thing(v2.value.value),
            list(map(lambda x: x.value, l1)),
            list(map(lambda x: x.value, l2)),
            host_o1,
            host_o2,
            host_result1,
            host_result2
        ) + 4

class ResourceAlias1(exports.ResourceAlias1):
    def a(self, f: exports.resource_alias1.Foo) -> List[resource_alias1.Thing]:
        return list(
            map(
                resource_alias1.wrap_thing,
                imports.resource_alias1.a(imports.resource_alias1.Foo(f.thing.value))
            )
        )

class ResourceAlias2(exports.ResourceAlias2):
    def b(self, f: exports.resource_alias2.Foo, g: exports.resource_alias1.Foo) -> List[resource_alias1.Thing]:
        return list(
            map(
                resource_alias1.wrap_thing,
                imports.resource_alias2.b(
                    imports.resource_alias2.Foo(f.thing.value),
                    exports.resource_alias1.Foo(g.thing.value)
                )
            )
        )

class ResourceBorrowInRecord(exports.ResourceBorrowInRecord):
    def test(self, a: List[exports.resource_borrow_in_record.Foo]) -> List[resource_borrow_in_record.Thing]:
        return list(
            map(
                resource_borrow_in_record.wrap_thing,
                imports.resource_borrow_in_record.test(
                    list(map(lambda x: imports.resource_borrow_in_record.Foo(x.thing.value), a))
                )
            )
        )

async def pipe_bytes(rx: ByteStreamReader, tx: ByteStreamWriter):
    while not (rx.writer_dropped or tx.reader_dropped):
        await tx.write_all(await rx.read(1024))

async def pipe_strings(rx: FutureReader[str], tx: StreamReader[str]):
    await tx.write(await rx.read())

async def pipe_things(rx: StreamReader[streams_and_futures.Thing], tx: StreamWriter[streams_and_futures.Thing]):
    # Read the things one at a time, forcing the host to re-take ownership of
    # any unwritten items between writes.
    things = []
    while not rx.writer_dropped:
        things += await rx.read(1)

    # Write the things all at once.  The host will read them only one at a time,
    # forcing us to re-take ownership of any unwritten items between writes.
    await tx.write_all(things)

async def pipe_host_things(rx: StreamReader[host_thing_interface.HostThing], tx: StreamWriter[host_thing_interface.HostThing]):
    # Read the things one at a time, forcing the host to re-take ownership of
    # any unwritten items between writes.
    things = []
    while not rx.writer_dropped:
        things += await rx.read(1)

    # Write the things all at once.  The host will read them only one at a time,
    # forcing us to re-take ownership of any unwritten items between writes.
    await tx.write_all(things)

async def write_thing(thing: my_streams_and_futures.Thing,
                      tx1: FutureWriter[streams_and_futures.Thing],
                      tx2: FutureWriter[streams_and_futures.Thing]):
    # The host will drop the first reader without reading, which should give us
    # back ownership of `thing`.
    wrote = await tx1.write(thing)
    assert not wrote
    # The host will read from the second reader, though.
    wrote = await tx2.write(thing)
    assert wrote

async def write_host_thing(thing: host_thing_interface.HostThing,
                      tx1: FutureWriter[host_thing_interface.HostThing],
                      tx2: FutureWriter[host_thing_interface.HostThing]):
    # The host will drop the first reader without reading, which should give us
    # back ownership of `thing`.
    wrote = await tx1.write(thing)
    assert not wrote
    # The host will read from the second reader, though.
    wrote = await tx2.write(thing)
    assert wrote

def unreachable() -> str:
    raise AssertionError
        
class StreamsAndFutures(exports.StreamsAndFutures):
    async def echo_stream_u8(self, stream: ByteStreamReader) -> ByteStreamReader:
        tx, rx = tests.byte_stream()
        componentize_py_async_support.spawn(pipe_bytes(stream, tx))
        return rx

    async def echo_future_string(self, future: FutureReader[str]) -> FutureReader[str]:
        tx, rx = tests.string_future(unreachable)
        componentize_py_async_support.spawn(pipe_strings(future, tx))
        return rx

    async def short_reads(self, stream: StreamReader[streams_and_futures.Thing]) -> StreamReader[streams_and_futures.Thing]:
        tx, rx = tests.streams_and_futures_thing_stream()
        componentize_py_async_support.spawn(pipe_things(stream, tx))
        return rx

    async def short_reads_host(self, stream: StreamReader[host_thing_interface.HostThing]) -> StreamReader[host_thing_interface.HostThing]:
        tx, rx = tests.host_thing_interface_host_thing_stream()
        componentize_py_async_support.spawn(pipe_host_things(stream, tx))
        return rx

    async def dropped_future_reader(self, value: str) -> tuple[FutureReader[streams_and_futures.Thing], FutureReader[streams_and_futures.Thing]]:
        tx1, rx1 = tests.streams_and_futures_thing_future(unreachable)
        tx2, rx2 = tests.streams_and_futures_thing_future(unreachable)
        componentize_py_async_support.spawn(write_thing(my_streams_and_futures.Thing(value), tx1, tx2))
        return (rx1, rx2)

    async def dropped_future_reader_host(self, value: str) -> tuple[FutureReader[host_thing_interface.HostThing], FutureReader[host_thing_interface.HostThing]]:
        tx1, rx1 = tests.host_thing_interface_host_thing_future(unreachable)
        tx2, rx2 = tests.host_thing_interface_host_thing_future(unreachable)
        componentize_py_async_support.spawn(write_host_thing(host_thing_interface.HostThing(value), tx1, tx2))
        return (rx1, rx2)

class Tests(tests.Tests):
    def test_resource_borrow_import(self, v: int) -> int:
        return resource_borrow_import.foo(resource_borrow_import.Thing(v + 1)) + 4

    def test_resource_alias(self, things: List[imports.resource_alias1.Thing]) -> List[imports.resource_alias1.Thing]:
        return things

    def add(self, a: imports.resource_floats.Float, b: imports.resource_floats.Float) -> imports.resource_floats.Float:
        return imports.resource_floats.Float(a.get() + b.get() + 5)

    def read_file(self, path: str) -> bytes:
        try:
            with open(file=path, mode="rb") as f:
                return f.read()
        except:
            raise Err(traceback.format_exc())

    def test_refcounts(self):
        # Retrieve 5GiB in chunks of 1MiB, which should _not_ lead to a
        # `MemoryError` if we're handling refcounts correctly in the runtime.
        for _ in range(5 * 1024):
            chunk = tests.get_bytes(1024 * 1024)
   
class FooInterface(foo_exports.FooInterface):
    def test(self, s: str) -> str:
        return foo_test(f"{s} FooInterface.test")

class BarInterface(bar_exports.BarInterface):
    def test(self, s: str) -> str:
        return bar_test(f"{s} BarInterface.test")

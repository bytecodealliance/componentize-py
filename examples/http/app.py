import asyncio
import poll_loop
import proxy
from proxy import exports
from proxy.types import Ok
from proxy.imports import types2 as types, streams2 as streams
from proxy.imports.types2 import MethodPost
from poll_loop import Stream, Sink, PollLoop

class IncomingHandler2(exports.IncomingHandler2):
    def handle(request: int, response_out: int):
        loop = PollLoop()
        asyncio.set_event_loop(loop)
        loop.run_until_complete(handle_async(request, response_out))

async def handle_async(request: int, response_out: int):
    method = types.incoming_request_method(request)
    path = types.incoming_request_path_with_query(request)
    headers = types.fields_entries(types.incoming_request_headers(request))

    if isinstance(method, MethodPost) and path == "/echo":
        response = types.new_outgoing_response(
            200,
            types.new_fields(list(filter(lambda pair: pair[0] == "content-type", headers)))
        )
        types.set_response_outparam(response_out, Ok(response))

        stream = Stream(types.incoming_request_consume(request))
        sink = Sink(types.outgoing_response_write(response))

        while True:
            chunk = await stream.next()
            if chunk is None:
                break
            else:
                await sink.send(chunk)

        sink.close()
    else:
        response = types.new_outgoing_response(400, types.new_fields([]))
        types.set_response_outparam(response_out, Ok(response))
        types.finish_outgoing_stream(types.outgoing_response_write(response))

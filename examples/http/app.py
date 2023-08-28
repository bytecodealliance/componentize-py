"""Demo of a serverless app using `wasi-http` to handle inbound HTTP requests.

This demonstrates how to use WASI's asynchronous capabilities to manage multiple
concurrent requests and streaming bodies.  It uses a custom `asyncio` event loop
to thread I/O through coroutines.
"""

# Note that we've temporarily renamed various `wasi-http` and `wasi-cli`
# interfaces (appending a 2 to each name) to avoid conflicting with the
# implementations in `wasmtime-wasi` which are still under development.
#
# Also note that `wasi-http` currently uses pseudo-resources (represented as
# integers) to model requests, responses, etc.  As of this writing, proper WIT
# resource support is still under development; we'll update this example to use
# them once they're ready.

import asyncio
import hashlib
import poll_loop

from proxy import exports
from proxy.types import Ok
from proxy.imports import types2 as types, outgoing_handler2 as outgoing_handler
from proxy.imports.types2 import MethodGet, MethodPost, Scheme, SchemeHttp, SchemeHttps, SchemeOther
from poll_loop import Stream, Sink, PollLoop
from typing import Tuple, cast
from urllib import parse

class IncomingHandler2(exports.IncomingHandler2):
    """Implements the `export`ed portion of the `wasi-http` `proxy` world."""
    
    def handle(self, request: int, response_out: int):
        """Handle the specified `request` (represented as a pseudo-resource), sending
        the response to `response_out`.
        """
        # Dispatch the request using `asyncio`, backed by a custom event loop
        # based on WASI's `poll_oneoff` function.
        loop = PollLoop()
        asyncio.set_event_loop(loop)
        loop.run_until_complete(handle_async(request, response_out))

async def handle_async(request: int, response_out: int):
    """Handle the specified `request` (represented as a pseudo-resource), sending
    the response to `response_out`."""
    
    method = types.incoming_request_method(request)
    path = types.incoming_request_path_with_query(request)
    headers = types.fields_entries(types.incoming_request_headers(request))

    if isinstance(method, MethodGet) and path == "/hash-all":
        # Collect one or more "url" headers, download their contents
        # concurrently, compute their SHA-256 hashes incrementally (i.e. without
        # buffering the response bodies), and stream the results back to the
        # client as they become available.
        
        urls = map(lambda pair: str(pair[1], "utf-8"), filter(lambda pair: pair[0] == "url", headers))

        response = types.new_outgoing_response(200, types.new_fields([("content-type", b"text/plain")]))

        types.set_response_outparam(response_out, Ok(response))
        
        sink = Sink(types.outgoing_response_write(response))

        for result in asyncio.as_completed(map(sha256, urls)):
            url, sha = await result
            await sink.send(bytes(f"{url}: {sha}\n", "utf-8"))

        sink.close()
    elif isinstance(method, MethodPost) and path == "/echo":
        # Echo the request body back to the client without buffering.
        
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

async def sha256(url: str) -> Tuple[str, str]:
    """Download the contents of the specified URL, computing the SHA-256
    incrementally as the response body arrives.

    This returns a tuple of the original URL and either the hex-encoded hash or
    an error message.
    """
    
    url_parsed = parse.urlparse(url)

    match url_parsed.scheme:
        case "http":
            scheme: Scheme = SchemeHttp()
        case "https":
            scheme = SchemeHttps()
        case _:
            scheme = SchemeOther(url_parsed.scheme)

    request = types.new_outgoing_request(
        MethodGet(),
        url_parsed.path,
        scheme,
        url_parsed.netloc,
        types.new_fields([])
    )

    response = await outgoing_request_send(request)

    status = types.incoming_response_status(response)
    if status < 200 or status > 299:
        return url, f"unexpected status: {status}"

    stream = Stream(types.incoming_response_consume(response))

    hasher = hashlib.sha256()
    while True:
        chunk = await stream.next()
        if chunk is None:
            return url, hasher.hexdigest()
        else:
            hasher.update(chunk)

async def outgoing_request_send(request: int) -> int:
    """Send the specified request and wait asynchronously for the response."""
    
    future = outgoing_handler.handle(request, None)
    pollable = types.listen_to_future_incoming_response(future)

    while True:
        response = types.future_incoming_response_get(future)
        if response is None:
            await poll_loop.register(cast(PollLoop, asyncio.get_event_loop()), pollable)
        else:
            if isinstance(response, Ok):
                return response.value
            else:
                raise response


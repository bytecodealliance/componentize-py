"""Demo of a serverless app using `wasi-http` to handle inbound HTTP requests.

This demonstrates how to use WASI's asynchronous capabilities to manage multiple
concurrent requests and streaming bodies.  It uses a custom `asyncio` event loop
to thread I/O through coroutines.
"""

import asyncio
import hashlib
import poll_loop

from proxy import exports
from proxy.types import Ok
from proxy.imports import types
from proxy.imports.types import (
    Method_Get, Method_Post, Scheme, Scheme_Http, Scheme_Https, Scheme_Other, IncomingRequest, ResponseOutparam,
    OutgoingResponse, Fields, OutgoingBody, OutgoingRequest
)
from poll_loop import Stream, Sink, PollLoop
from typing import Tuple
from urllib import parse

class IncomingHandler(exports.IncomingHandler):
    """Implements the `export`ed portion of the `wasi-http` `proxy` world."""

    def handle(self, request: IncomingRequest, response_out: ResponseOutparam):
        """Handle the specified `request`, sending the response to `response_out`.

        """
        # Dispatch the request using `asyncio`, backed by a custom event loop
        # based on WASI's `poll_oneoff` function.
        loop = PollLoop()
        asyncio.set_event_loop(loop)
        loop.run_until_complete(handle_async(request, response_out))

async def handle_async(request: IncomingRequest, response_out: ResponseOutparam):
    """Handle the specified `request`, sending the response to `response_out`."""

    method = request.method()
    path = request.path_with_query()
    headers = request.headers().entries()

    if isinstance(method, Method_Get) and path == "/hash-all":
        # Collect one or more "url" headers, download their contents
        # concurrently, compute their SHA-256 hashes incrementally (i.e. without
        # buffering the response bodies), and stream the results back to the
        # client as they become available.

        urls = map(lambda pair: str(pair[1], "utf-8"), filter(lambda pair: pair[0] == "url", headers))

        response = OutgoingResponse(Fields.from_list([("content-type", b"text/plain")]))

        response_body = response.body()

        ResponseOutparam.set(response_out, Ok(response))

        sink = Sink(response_body)
        for result in asyncio.as_completed(map(sha256, urls)):
            url, sha = await result
            await sink.send(bytes(f"{url}: {sha}\n", "utf-8"))

        sink.close()

    elif isinstance(method, Method_Post) and path == "/echo":
        # Echo the request body back to the client without buffering.

        response = OutgoingResponse(Fields.from_list(list(filter(lambda pair: pair[0] == "content-type", headers))))

        response_body = response.body()

        ResponseOutparam.set(response_out, Ok(response))

        stream = Stream(request.consume())
        sink = Sink(response_body)
        while True:
            chunk = await stream.next()
            if chunk is None:
                break
            else:
                await sink.send(chunk)

        sink.close()
    else:
        response = OutgoingResponse(Fields.from_list([]))
        response.set_status_code(400)
        body = response.body()
        ResponseOutparam.set(response_out, Ok(response))
        OutgoingBody.finish(body, None)

async def sha256(url: str) -> Tuple[str, str]:
    """Download the contents of the specified URL, computing the SHA-256
    incrementally as the response body arrives.

    This returns a tuple of the original URL and either the hex-encoded hash or
    an error message.
    """

    url_parsed = parse.urlparse(url)

    match url_parsed.scheme:
        case "http":
            scheme: Scheme = Scheme_Http()
        case "https":
            scheme = Scheme_Https()
        case _:
            scheme = Scheme_Other(url_parsed.scheme)

    request = OutgoingRequest(Fields.from_list([]))
    request.set_scheme(scheme)
    request.set_authority(url_parsed.netloc)
    request.set_path_with_query(url_parsed.path)

    response = await poll_loop.send(request)
    status = response.status()
    if status < 200 or status > 299:
        return url, f"unexpected status: {status}"

    stream = Stream(response.consume())
    hasher = hashlib.sha256()
    while True:
        chunk = await stream.next()
        if chunk is None:
            return url, hasher.hexdigest()
        else:
            hasher.update(chunk)

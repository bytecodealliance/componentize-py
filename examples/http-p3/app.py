"""Demo of a serverless app using `wasi-http` to handle inbound HTTP requests.

This demonstrates how to use WASI's asynchronous capabilities to manage multiple
concurrent requests and streaming bodies.  It uses a custom `asyncio` event loop
to thread I/O through coroutines.
"""

import asyncio
import hashlib
import componentize_py_async_support
import wit_world

from typing import Optional
from componentize_py_types import Ok, Result
from componentize_py_async_support.streams import ByteStreamWriter
from componentize_py_async_support.futures import FutureReader
from wit_world import exports
from wit_world.imports import handler
from wit_world.imports.wasi_http_types import (
    Method_Get,
    Method_Post,
    Scheme,
    Scheme_Http,
    Scheme_Https,
    Scheme_Other,
    Request,
    Response,
    Fields,
    ErrorCode
)
from urllib import parse


class Handler(exports.Handler):
    """Implements the `export`ed portion of the `wasi-http` `proxy` world."""

    async def handle(self, request: Request) -> Response:
        """Handle the specified `request`, returning a `Response`."""
        
        method = request.get_method()
        path = request.get_path_with_query()
        headers = request.get_headers().copy_all()

        if isinstance(method, Method_Get) and path == "/hash-all":
            # Collect one or more "url" headers, download their contents
            # concurrently, compute their SHA-256 hashes incrementally (i.e. without
            # buffering the response bodies), and stream the results back to the
            # client as they become available.

            urls = list(map(
                lambda pair: str(pair[1], "utf-8"),
                filter(lambda pair: pair[0] == "url", headers),
            ))

            tx, rx = wit_world.byte_stream()
            componentize_py_async_support.spawn(hash_all(urls, tx))

            return Response.new(
                Fields.from_list([("content-type", b"text/plain")]),
                rx,
                trailers_future()
            )[0]

        elif isinstance(method, Method_Post) and path == "/echo":
            # Echo the request body back to the client without buffering.

            rx, trailers = Request.consume_body(request, unit_future())

            return Response.new(
                Fields.from_list(
                    list(filter(lambda pair: pair[0] == "content-type", headers))
                ),
                rx,
                trailers
            )[0]

        else:
            response = Response.new(Fields(), None, trailers_future())[0]
            response.set_status_code(400)
            return response


async def hash_all(urls: list[str], tx: ByteStreamWriter) -> None:
    with tx:
        for result in asyncio.as_completed(map(sha256, urls)):
            url, sha = await result
            await tx.write_all(bytes(f"{url}: {sha}\n", "utf-8"))
            
            
async def sha256(url: str) -> tuple[str, str]:
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

    request = Request.new(Fields(), None, trailers_future(), None)[0]
    request.set_scheme(scheme)
    request.set_authority(url_parsed.netloc)
    request.set_path_with_query(url_parsed.path)

    response = await handler.handle(request)
    status = response.get_status_code()
    if status < 200 or status > 299:
        return url, f"unexpected status: {status}"

    rx = Response.consume_body(response, unit_future())[0]
    
    hasher = hashlib.sha256()
    with rx:
        while not rx.writer_dropped:
            chunk = await rx.read(16 * 1024)
            hasher.update(chunk)

    return url, hasher.hexdigest()


def trailers_future() -> FutureReader[Result[Optional[Fields], ErrorCode]]:
    return wit_world.result_option_wasi_http_types_fields_wasi_http_types_error_code_future(lambda: Ok(None))[1]


def unit_future() -> FutureReader[Result[None, ErrorCode]]:
    return wit_world.result_unit_wasi_http_types_error_code_future(lambda: Ok(None))[1]

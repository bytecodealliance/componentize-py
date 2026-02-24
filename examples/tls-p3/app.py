import sys
import asyncio
import wit_world
from wit_world import exports
from wit_world.imports.wasi_sockets_types import (
    TcpSocket,
    IpSocketAddress_Ipv4,
    IpSocketAddress_Ipv6,
    Ipv4SocketAddress,
    Ipv6SocketAddress,
    IpAddressFamily,
    IpAddress_Ipv4,
    IpAddress_Ipv6,
)
from wit_world.imports.ip_name_lookup import resolve_addresses
from wit_world.imports.client import Connector


class Run(exports.Run):
    async def run(self) -> None:
        args = sys.argv[1:]
        if len(args) != 1:
            print("usage: tls-p3 <server_name>", file=sys.stderr)
            exit(-1)

        server_name = args[0]
        await send_and_receive(server_name)


async def send_and_receive(server_name: str) -> None:
    port = 443
    addresses = await resolve_addresses(server_name)
    address = addresses[0]

    if isinstance(address, IpAddress_Ipv4):
        family = IpAddressFamily.IPV4
        sock_addr: IpSocketAddress_Ipv4 | IpSocketAddress_Ipv6 = IpSocketAddress_Ipv4(
            Ipv4SocketAddress(port=port, address=address.value)
        )
    else:
        family = IpAddressFamily.IPV6
        sock_addr = IpSocketAddress_Ipv6(
            Ipv6SocketAddress(port=port, flow_info=0, address=address.value, scope_id=0)
        )

    sock = TcpSocket.create(family)
    await sock.connect(sock_addr)

    tls = Connector()

    data_send_tx, data_send_rx = wit_world.byte_stream()
    tls_send_rx, tls_send_fut = tls.send(data_send_rx)
    sock_send_fut = sock.send(tls_send_rx)

    tls_recv_rx, sock_recv_fut = sock.receive()
    data_recv_rx, tls_recv_fut = tls.receive(tls_recv_rx)

    async def write() -> None:
        with data_send_tx:
            await data_send_tx.write_all(f"GET / HTTP/1.1\r\nHost: {server_name}\r\nUser-Agent: wasmtime-wasi-rust\r\nConnection: close\r\n\r\n".encode())

    async def read() -> None:
        with data_recv_rx:
            while not data_recv_rx.writer_dropped:
                buf = await data_recv_rx.read(1024)
                sys.stdout.buffer.write(buf)

    await asyncio.gather(
        Connector.connect(tls, server_name),
        write(),
        read(),
        sock_send_fut.read(),
        sock_recv_fut.read(),
        tls_send_fut.read(),
        tls_recv_fut.read(),
    )

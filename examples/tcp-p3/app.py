import sys
import asyncio
import ipaddress
from ipaddress import IPv4Address, IPv6Address
import wit_world
from wit_world import exports
from wit_world.imports.wasi_sockets_types import (
    TcpSocket,
    IpSocketAddress_Ipv4,
    IpSocketAddress_Ipv6,
    Ipv4SocketAddress,
    Ipv6SocketAddress,
    IpAddressFamily,
)
from typing import Tuple


IPAddress = IPv4Address | IPv6Address

class Run(exports.Run):
    async def run(self) -> None:
        args = sys.argv[1:]
        if len(args) != 1:
            print("usage: tcp-p3 <address>:<port>", file=sys.stderr)
            exit(-1)

        address, port = parse_address_and_port(args[0])
        await send_and_receive(address, port)


def parse_address_and_port(address_and_port: str) -> Tuple[IPAddress, int]:
    ip, separator, port = address_and_port.rpartition(":")
    assert separator
    return (ipaddress.ip_address(ip.strip("[]")), int(port))


def make_socket_address(address: IPAddress, port: int) -> IpSocketAddress_Ipv4 | IpSocketAddress_Ipv6:
    if isinstance(address, IPv4Address):
        octets = address.packed
        return IpSocketAddress_Ipv4(Ipv4SocketAddress(
            port=port,
            address=(octets[0], octets[1], octets[2], octets[3]),
        ))
    else:
        b = address.packed
        return IpSocketAddress_Ipv6(Ipv6SocketAddress(
            port=port,
            flow_info=0,
            address=(
                (b[0] << 8) | b[1],
                (b[2] << 8) | b[3],
                (b[4] << 8) | b[5],
                (b[6] << 8) | b[7],
                (b[8] << 8) | b[9],
                (b[10] << 8) | b[11],
                (b[12] << 8) | b[13],
                (b[14] << 8) | b[15],
            ),
            scope_id=0,
        ))


async def send_and_receive(address: IPAddress, port: int) -> None:
    family = IpAddressFamily.IPV4 if isinstance(address, IPv4Address) else IpAddressFamily.IPV6

    sock = TcpSocket.create(family)

    await sock.connect(make_socket_address(address, port))

    send_tx, send_rx = wit_world.byte_stream()
    async def write() -> None:
        await send_tx.write_all(b"hello, world!")

    recv_rx, recv_fut = sock.receive()
    async def read() -> None:
        with recv_rx:
            data = await recv_rx.read(1024)
            print(f"received: {str(data)}")
            send_tx.__exit__(None, None, None)
    await asyncio.gather(recv_fut.read(), read(), sock.send(send_rx), write())

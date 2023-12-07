import sys
import asyncio
import ipaddress
from ipaddress import IPv4Address, IPv6Address
from command import exports
from typing import Tuple

class Run(exports.Run):
    def run(self):
        args = sys.argv[1:]
        if len(args) != 1:
            print(f"usage: tcp <address>:<port>", file=sys.stderr)
            exit(-1)

        address, port = parse_address_and_port(args[0])
        asyncio.run(send_and_receive(address, port))

IPAddress = IPv4Address | IPv6Address
        
def parse_address_and_port(address_and_port: str) -> Tuple[IPAddress, int]:
    ip, separator, port = address_and_port.rpartition(':')
    assert separator
    return (ipaddress.ip_address(ip.strip("[]")), int(port))
        
async def send_and_receive(address: IPAddress, port: int):
    rx, tx = await asyncio.open_connection(str(address), port)

    tx.write(b"hello, world!")
    await tx.drain()

    data = await rx.read(1024)
    print(f"received: {str(data)}")

    tx.close()
    await tx.wait_closed()
    

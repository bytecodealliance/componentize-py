from wit.exports.wasi.cli_v0_3 import run, Run

@run.guest
class Cli(Run):
    async def run(self) -> None:
        print("Hello, world!")

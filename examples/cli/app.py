from wit.exports.wasi.cli_v0_2 import run, Run

@run.guest
class Cli(Run):
    def run(self) -> None:
        print("Hello, world!")

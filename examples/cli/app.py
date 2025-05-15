from wit_world import exports
from wit_world.imports.environment import get_arguments
import pdb

class Run(exports.Run):
    def run(self) -> None:
        if "--pdb" in get_arguments():
            pdb.set_trace()
        print("Hello, world!")

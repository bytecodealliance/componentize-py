import time

from threading import Thread
from wit.exports.wasi.cli_v0_3 import run, Run

@run.guest
class Cli(Run):
    async def run(self) -> None:
        threads = list(map(lambda name: Thread(target=run_thread, args=(name,)), ["a", "b", "c"]))
        
        for thread in threads:
            thread.start()
            
        print("started all threads")
            
        for thread in threads:
            thread.join()
            
        print("joined all threads")

def run_thread(name: str) -> None:
    print(f"thread `{name}` started")
    time.sleep(1)
    print(f"thread `{name}` finished")    

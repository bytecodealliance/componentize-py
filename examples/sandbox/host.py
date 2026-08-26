from wasmtime import Config, Engine, Store
from wasmtime.component import Component, Linker
import json
import sys
from threading import Timer
from typing import List, Tuple

TIMEOUT_SECONDS = 20
MEMORY_LIMIT_BYTES = 40 * 1024 * 1024

args = sys.argv[1:]
if len(args) == 0:
    print("usage: python3 host.py [<statement>...] <expression>", file=sys.stderr)
    exit(-1)

config = Config()
config.epoch_interruption = True
config.cache = True

def on_timeout(engine):
    print("timeout!")
    engine.increment_epoch()

engine = Engine(config)
timer = Timer(TIMEOUT_SECONDS, on_timeout, args=(engine,))
timer.start()

try:
    store = Store(engine)
    store.set_epoch_deadline(1)
    store.set_limits(memory_size=MEMORY_LIMIT_BYTES)

    component = Component.from_file(engine, "sandbox.wasm")
    linker = Linker(engine)
    instance = linker.instantiate(store, component)
    sandbox_exec = instance.get_func(store, "exec")
    sandbox_eval = instance.get_func(store, "eval")

    for arg in args[:-1]:
        result = sandbox_exec(store, arg)
        if isinstance(result, str):
            print(f"exec error: {result}")
            exit(-1)

    result = sandbox_eval(store, args[-1])
    if result.tag == "ok":
        result = json.loads(result.payload)
        print(f"result: {result}")
    else:
        print(f"eval error: {result.payload}")

finally:
    timer.cancel()

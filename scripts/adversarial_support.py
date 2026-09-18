"""Reproducible, bounded test-only generators; failures retain only replay metadata."""

import hashlib
import os
from pathlib import Path
import random


def number(name, default, maximum):
    value = int(os.environ.get(name, str(default)))
    if not 0 <= value <= maximum:
        raise ValueError(f"{name} exceeds the test budget")
    return value


def run(name, property_check):
    seed = number("BALUN_ADVERSARIAL_SEED", 20260917, (1 << 63) - 1)
    cases = number("BALUN_ADVERSARIAL_CASES", 128, 100000)
    first = number("BALUN_ADVERSARIAL_START", 0, 100000)
    if not cases or first + cases > 100000:
        raise ValueError("invalid adversarial case range")
    domain = int.from_bytes(hashlib.sha256(name.encode()).digest()[:8], "little")
    for case in range(first, first + cases):
        generator = random.Random(seed ^ domain ^ (case * 0x9e3779b97f4a7c15))
        try:
            property_check(generator)
        except Exception:
            replay = (f"suite={name}\nBALUN_ADVERSARIAL_SEED={seed}\n"
                      f"BALUN_ADVERSARIAL_START={case}\nBALUN_ADVERSARIAL_CASES=1\n")
            print(replay, flush=True)
            directory = os.environ.get("BALUN_ADVERSARIAL_FAILURE_DIR")
            if directory:
                path = Path(directory)
                path.mkdir(parents=True, exist_ok=True)
                (path / f"{name}-{seed}-{case}.txt").write_text(replay, encoding="utf-8")
            raise
    print(f"adversarial {name}: seed={seed}, start={first}, cases={cases}")


def mutate(generator, seed, limit):
    data = bytearray(seed[:limit])
    for _ in range(generator.randrange(1, 9)):
        operation = generator.randrange(4)
        if operation == 0 and data:
            data[generator.randrange(len(data))] ^= 1 << generator.randrange(8)
        elif operation == 1 and data:
            del data[generator.randrange(len(data))]
        elif operation == 2 and len(data) < limit:
            data.insert(generator.randrange(len(data) + 1), generator.randrange(256))
        else:
            del data[generator.randrange(len(data) + 1):]
    return data

#!/usr/bin/env python3
"""Compare Rust's native volume observations with an independent wire decoder."""

import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys


CRATE = Path(__file__).resolve().parents[1]
FIXTURES = CRATE / "tests" / "fixtures"
GEOMETRY_OFFSET = 40
FIRST_FORK_OFFSET = 112
FORK_BYTES = 80
ALTERNATE_TAIL_BYTES = 1024
EMBEDDED_EXTENTS = 8
FORK_ROLES = ("allocation", "extents", "catalog", "attributes", "startup")
LEDGERS = ("native-hfs-plus.json", "native-optical-hfs-plus.json", "native-header-matrix.json")
TEST = "reports_native_volume_fields_for_the_wire_reference"


def native_sources(ledgers):
    native, optical, matrix = ledgers
    for name in ("native-primary.bin", "native-alternate.bin"):
        yield name, native["raw_length"], native["fixtures"][name]["sha256"]
    yield "optical-primary.bin", optical["volume_bytes"], optical["fixtures"]["optical-primary.bin"]["sha256"]
    for name, record in matrix["fixtures"].items():
        yield name, record["raw_bytes"], record["header_sha256"]


def expected_rows(ledgers):
    rows = []
    for name, device_bytes, digest in native_sources(ledgers):
        wire = (FIXTURES / name).read_bytes()
        if hashlib.sha256(wire).hexdigest() != digest:
            raise ValueError(f"fixture digest mismatch for {name}")
        signature = {b"H+": "HfsPlus", b"HX": "HfsX"}[wire[:2]]
        block_bytes, total, free, next_block = struct.unpack_from(">4I", wire, GEOMETRY_OFFSET)
        rows.append(f"HFS+ header {name} {signature} {block_bytes} {total} {free} {next_block} "
                    f"{block_bytes * total} {device_bytes - ALTERNATE_TAIL_BYTES}")
        for index, role in enumerate(FORK_ROLES):
            offset = FIRST_FORK_OFFSET + index * FORK_BYTES
            logical, clump, allocated = struct.unpack_from(">QII", wire, offset)
            values = struct.unpack_from(f">{2 * EMBEDDED_EXTENTS}I", wire, offset + struct.calcsize(">QII"))
            extents = [f"{start}:{length}" for start, length in zip(values[::2], values[1::2]) if length]
            rows.append(" ".join([f"HFS+ fork {name} {role}", str(logical), str(clump), str(allocated), *extents]))
    return rows


def main():
    tool = shutil.which("rustc")
    if tool is None:
        raise SystemExit("rustc is required")
    ledgers = [(FIXTURES / name).read_bytes() for name in LEDGERS]
    expected = expected_rows([json.loads(ledger) for ledger in ledgers])
    if len(expected) != 36:
        raise SystemExit("expected six complete volume descriptor groups")
    env = os.environ | {"RUSTC": tool}
    print(f"Python {sys.version.split()[0]}", flush=True)
    subprocess.run(["cargo", "--version"], check=True)
    subprocess.run([tool, "--version"], check=True)
    run = subprocess.run(["cargo", "test", "--color", "never", "--locked", "--offline", "--no-default-features",
                          "--manifest-path", str(CRATE / "Cargo.toml"), "--test", "encode_volume", "--",
                          "--exact", TEST, "--show-output"], env=env, text=True, stdout=subprocess.PIPE,
                         stderr=subprocess.STDOUT, check=False)
    rows = [line for line in run.stdout.splitlines() if line.startswith(("HFS+ header ", "HFS+ fork "))]
    if run.returncode or rows != expected:
        print(run.stdout, end="")
        raise SystemExit("native wire reference comparison failed")
    print(f"Verified native fixture ledgers SHA-256 {hashlib.sha256(b''.join(ledgers)).hexdigest()}")
    print("\n".join(rows))
    print(f"Compared {len(rows)} native wire rows SHA-256 {hashlib.sha256(chr(10).join(rows).encode()).hexdigest()}")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Derive portable HFS+ ordering rules from public macOS comparison behavior."""

import argparse
import ctypes
from functools import cmp_to_key
from hashlib import sha256
from itertools import pairwise
from pathlib import Path
from random import Random
import sys

CORE_SERVICES = "/System/Library/Frameworks/CoreServices.framework/CoreServices"
HFS_EXTENDED_ORDER = 1
ORDER_FIELD_SHIFT = 24
MAX_UNIT = 0xFFFF
IGNORABLE_WEIGHT = 0
FUZZ_SEED = 0x4846532B
RANDOM_PAIRS = 20_000
MAX_NAME_UNITS = 255
CRATE = Path(__file__).resolve().parents[1]
RULES = CRATE / "src/names/collation/ordinal_overrides.rs"
FIXTURE = CRATE / "tests/fixtures/native-collation.tsv"

# Raw UTF-16 name pairs. These probe empty keys, historical case classes,
# ignorable and significant formatting units, surrogate pairs, and ordering
# where an uppercase character changes position relative to punctuation.
SCENARIOS = (
    ((), ()),
    ((), (0x0020,)),
    ((0x0041,), (0x0061,)),
    ((0x0041,), (0x005F,)),
    ((0x005A,), (0x005B,)),
    ((0x0061,), (0x0061, 0x0020)),
    ((0x0045, 0x0301), (0x0065, 0x0301)),
    ((0x0045, 0x0301), (0x0065, 0x0300)),
    ((0x0065,), (0x0065, 0x0301)),
    ((0x00C6,), (0x00E6,)),
    ((0x0181,), (0x0253,)),
    ((0x01C4,), (0x01C5,)),
    ((0x01C5,), (0x01C6,)),
    ((0x03A3,), (0x03C3,)),
    ((0x03A3,), (0x03C2,)),
    ((0x0410,), (0x0430,)),
    ((0x0531,), (0x0561,)),
    ((0x10A0,), (0x10D0,)),
    ((0x10A0,), (0x2D00,)),
    ((0x2160,), (0x2170,)),
    ((0xFF21,), (0xFF41,)),
    ((0xFF21,), (0x0061,)),
    ((0x200C,), ()),
    ((0x200D,), ()),
    ((0x202E,), ()),
    ((0x206F,), ()),
    ((0xFEFF,), ()),
    ((0x200B,), ()),
    ((0x202F,), ()),
    ((0x2069,), ()),
    ((0xFE0F,), ()),
    ((0x0061, 0x200C, 0x0062), (0x0041, 0x0062, 0xFEFF)),
    ((0xD801, 0xDC00), (0xD801, 0xDC28)),
    ((0xD83D, 0xDE00), (0xD83D, 0xDE01)),
    ((0xD800,), (0xD801,)),
    ((0x0000,), (0xFFFE,)),
    ((0x0000,), (0xFFFF,)),
    ((0x0000, 0x0041), (0xFFFF, 0x0062)),
)


class Oracle:
    def __init__(self):
        self.library = ctypes.CDLL(CORE_SERVICES)
        self.function = self.library.UCCompareTextNoLocale
        char_pointer = ctypes.POINTER(ctypes.c_uint16)
        self.function.argtypes = (
            ctypes.c_uint32,
            char_pointer,
            ctypes.c_ulong,
            char_pointer,
            ctypes.c_ulong,
            ctypes.POINTER(ctypes.c_uint8),
            ctypes.POINTER(ctypes.c_int32),
        )
        self.function.restype = ctypes.c_int32

    def compare(self, left, right):
        left_units = (ctypes.c_uint16 * len(left))(*left)
        right_units = (ctypes.c_uint16 * len(right))(*right)
        equivalent = ctypes.c_uint8()
        order = ctypes.c_int32()
        status = self.function(
            HFS_EXTENDED_ORDER << ORDER_FIELD_SHIFT,
            left_units,
            len(left),
            right_units,
            len(right),
            ctypes.byref(equivalent),
            ctypes.byref(order),
        )
        if status or (not equivalent.value and not order.value):
            raise RuntimeError(f"native comparison failed ({status}): {left!r}, {right!r}")
        # The API can give a secondary order for two equivalent names.
        if equivalent.value:
            return 0
        return -1 if order.value < 0 else 1


def derive_overrides(oracle):
    singleton_order = sorted(
        ((unit,) for unit in range(MAX_UNIT + 1)), key=cmp_to_key(oracle.compare)
    )
    groups = [[singleton_order[0][0]]]
    for left, right in pairwise(singleton_order):
        order = oracle.compare(left, right)
        if oracle.compare(right, left) != -order:
            raise RuntimeError("native singleton ordering was not symmetric")
        if order == 0:
            groups[-1].append(right[0])
        elif order < 0:
            groups.append([right[0]])
        else:
            raise RuntimeError("native singleton groups were unsorted")
    if oracle.compare(tuple(groups[0]), ()) != 0:
        raise RuntimeError("native ignorables did not form the first singleton class")
    overrides = {unit: IGNORABLE_WEIGHT for unit in groups[0]}
    previous = IGNORABLE_WEIGHT
    for group in groups[1:]:
        representative = max(group)
        if representative <= previous:
            raise RuntimeError("native class representatives were unsorted")
        for unit in group:
            if oracle.compare((unit,), (representative,)) != 0:
                raise RuntimeError("native singleton class was inconsistent")
            if unit != representative:
                overrides[unit] = representative
        previous = representative
    return overrides


def validate_sequences(oracle, overrides):
    def key(units):
        return tuple(
            weight for unit in units if (weight := overrides.get(unit, unit)) != IGNORABLE_WEIGHT
        )

    def verify(left, right):
        a, b = key(left), key(right)
        order = (a > b) - (a < b)
        if oracle.compare(left, right) != order:
            raise RuntimeError(f"native sequence disagreed with unit model: {left!r}, {right!r}")

    rand = Random(FUZZ_SEED)
    changed = tuple(sorted(overrides))
    complete = range(MAX_UNIT + 1)
    for alphabet in (complete, changed):
        for _ in range(RANDOM_PAIRS):
            shared = tuple(rand.choices(alphabet, k=rand.randrange(MAX_NAME_UNITS)))
            bound = MAX_NAME_UNITS - len(shared) + 1
            left = shared + tuple(rand.choices(alphabet, k=rand.randrange(bound)))
            right = shared + tuple(rand.choices(alphabet, k=rand.randrange(bound)))
            verify(left, right)
    for unit, weight in overrides.items():
        equivalent = () if weight == IGNORABLE_WEIGHT else (weight,)
        # Check that an equivalence survives within a nonempty name.
        verify((ord("A"), unit, ord("b")), (ord("a"),) + equivalent + (ord("B"),))
    for left, right in SCENARIOS:
        verify(left, right)
        verify(right, left)


def render_rules(overrides):
    lines = [
        "// Generated by tools/capture_native_collation.py from observed comparisons.",
        "// Each pair maps a unit to its class representative. See UNICODE32.md.",
        "",
        "pub(super) const ORDINAL_OVERRIDES: &[(u16, u16)] = &[",
    ]
    lines.extend(f"\t(0x{unit:04x}, 0x{value:04x})," for unit, value in sorted(overrides.items()))
    lines.append("];")
    return "\n".join(lines) + "\n"


def render_fixture(oracle):
    def units(values):
        return " ".join(f"{value:04x}" for value in values)

    lines = ["# Source: public CoreServices HFS Extended profile. See UNICODE32.md."]
    lines.append("# Left UTF-16 units\tRight UTF-16 units\tHFS+ ordering")
    for left, right in SCENARIOS:
        lines.append(f"{units(left)}\t{units(right)}\t{oracle.compare(left, right)}")
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="compare with checked-in artifacts")
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("the native comparison oracle requires macOS")
    oracle = Oracle()
    overrides = derive_overrides(oracle)
    validate_sequences(oracle, overrides)
    for path, contents in ((RULES, render_rules(overrides)), (FIXTURE, render_fixture(oracle))):
        if args.check:
            if path.read_text(encoding="utf-8") != contents:
                parser.error(f"native observation changed: {path.relative_to(CRATE)}")
        else:
            path.write_text(contents, encoding="utf-8")
    if args.check:
        ignored = sum(weight == IGNORABLE_WEIGHT for weight in overrides.values())
        print(f"CoreServices HFS+ model: {MAX_UNIT + 1} singleton units, {len(overrides)} overrides, {ignored} ignorables")
        print(f"CoreServices HFS+ samples: {2 * RANDOM_PAIRS} seeded pairs, {len(overrides)} embedded overrides, "
              f"{2 * len(SCENARIOS)} directed pairs, at most {MAX_NAME_UNITS} units, seed {FUZZ_SEED:#x}")
        print(f"CoreServices HFS+ rules SHA-256: {sha256(RULES.read_bytes()).hexdigest()}")
        print(f"CoreServices HFS+ fixture SHA-256: {sha256(FIXTURE.read_bytes()).hexdigest()}")


if __name__ == "__main__":
    main()

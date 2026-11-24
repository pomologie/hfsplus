#!/usr/bin/env python3
"""Capture synthetic HFS+ file names from a macOS-generated catalog."""

import argparse
from pathlib import Path
import struct
import subprocess
import tempfile

AGREEMENT_CASES = (
    "Å",
    "\u212b",
    "豈",
    "\U0002f800",
    "\U0001d15e",
    "\U0001f600",
    "각",
    "A\u0301\u0327",
    "A\u20e1\u0327",
    "¾",
    "\U000110ba",
    "가",
    "힣",
    "A\u0301\u0300\u0327",
    "a\u0301\U0001d165",
    "\u0344",
    "\u2329",
)
NEWER_OPTICAL_CASES = (
    "A\u0301\u1ab0\u0327",
    "\u1b06",
    "\U0001109a",
)
AGREEMENT = "agreement"
NEWER_OPTICAL = "newer_optical"
VOLUME_NAME = "Unicode 3.2 Fixture"
DRIVER_BLOCK_BYTES_OFFSET = 2
PARTITION_MAP_ENTRY_COUNT_OFFSET = 4
PARTITION_FIRST_BLOCK_OFFSET = 8
PARTITION_TYPE = slice(48, 80)
HFS_PARTITION_TYPE = b"Apple_HFS"
VOLUME_HEADER_OFFSET = 1024
VOLUME_BLOCK_BYTES_OFFSET = 40
CATALOG_FORK_OFFSET = 272
CATALOG_EXTENT_FIRST_BLOCK_OFFSET = 16
TREE_FIRST_LEAF_NODE_OFFSET = 24
TREE_NODE_BYTES_OFFSET = 32
NODE_FORWARD_LINK_OFFSET = 0
NODE_RECORD_COUNT_OFFSET = 10
FILE_RECORD_TYPE = 2
RECORD_NAME_UNITS_OFFSET = 6
RECORD_NAME_OFFSET = 8
UTF16_UNIT_BYTES = 2
OFFSET_BYTES = 2
KEY_LENGTH_BYTES = 2
U32_BYTES = 4
CASE_ID_AND_SEPARATOR_LENGTH = 3


def number(data, offset, width):
    return int.from_bytes(data[offset:offset + width], "big")


def catalog_file_names(image):
    partition_block = number(image, DRIVER_BLOCK_BYTES_OFFSET, OFFSET_BYTES)
    entry_count = number(image, partition_block + PARTITION_MAP_ENTRY_COUNT_OFFSET, U32_BYTES)
    for index in range(1, entry_count + 1):
        entry_start = index * partition_block
        entry = image[entry_start:entry_start + partition_block]
        if entry[PARTITION_TYPE].rstrip(b"\0") == HFS_PARTITION_TYPE:
            volume = number(entry, PARTITION_FIRST_BLOCK_OFFSET, U32_BYTES) * partition_block
            break
    else:
        raise RuntimeError("no HFS+ partition in generated image")

    header = volume + VOLUME_HEADER_OFFSET
    block_size = number(image, header + VOLUME_BLOCK_BYTES_OFFSET, U32_BYTES)
    catalog_extent = header + CATALOG_FORK_OFFSET + CATALOG_EXTENT_FIRST_BLOCK_OFFSET
    catalog = volume + block_size * number(image, catalog_extent, U32_BYTES)
    node_size = number(image, catalog + TREE_NODE_BYTES_OFFSET, OFFSET_BYTES)
    leaf = number(image, catalog + TREE_FIRST_LEAF_NODE_OFFSET, U32_BYTES)
    seen = set()
    while leaf:
        if leaf in seen:
            raise RuntimeError("cycle in generated catalog")
        seen.add(leaf)
        node = catalog + leaf * node_size
        count = number(image, node + NODE_RECORD_COUNT_OFFSET, OFFSET_BYTES)
        for index in range(1, count + 1):
            at = node + number(image, node + node_size - OFFSET_BYTES * index, OFFSET_BYTES)
            key_length = number(image, at, KEY_LENGTH_BYTES)
            if number(image, at + key_length + KEY_LENGTH_BYTES, OFFSET_BYTES) != FILE_RECORD_TYPE:
                continue
            unit_count = number(image, at + RECORD_NAME_UNITS_OFFSET, OFFSET_BYTES)
            start = at + RECORD_NAME_OFFSET
            yield image[start:start + unit_count * UTF16_UNIT_BYTES].decode("utf-16be")
        leaf = number(image, node + NODE_FORWARD_LINK_OFFSET, U32_BYTES)


def units(value):
    return " ".join(f"{unit:04x}" for (unit,) in struct.iter_unpack(">H", value.encode("utf-16be")))


def capture():
    with tempfile.TemporaryDirectory(prefix="hfsplus-native-names-") as directory:
        root = Path(directory)
        entries = root / "entries"
        entries.mkdir()
        image = root / "fixture.dmg"
        input_names = [
            f"{index:02d}-{name}" for index, name in enumerate(AGREEMENT_CASES + NEWER_OPTICAL_CASES)
        ]
        for name in input_names:
            (entries / name).touch(exist_ok=False)
        subprocess.run([
            "/usr/bin/hdiutil", "makehybrid", "-hfs", "-hfs-volume-name", VOLUME_NAME,
            "-o", str(image), str(entries),
        ], check=True, stdout=subprocess.DEVNULL)
        by_prefix = {name[:CASE_ID_AND_SEPARATOR_LENGTH]: name for name in catalog_file_names(image.read_bytes())}
        if len(by_prefix) != len(input_names):
            raise RuntimeError("native catalog did not contain one file per case")
        lines = ["# Comparison\tInput UTF-16 units\tNative catalog UTF-16 units"]
        for index, name in enumerate(input_names):
            comparison = AGREEMENT if index < len(AGREEMENT_CASES) else NEWER_OPTICAL
            observed = by_prefix[name[:CASE_ID_AND_SEPARATOR_LENGTH]]
            lines.append(f"{comparison}\t{units(name)}\t{units(observed)}")
        return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    args.output.write_text(capture(), encoding="utf-8")


if __name__ == "__main__":
    main()

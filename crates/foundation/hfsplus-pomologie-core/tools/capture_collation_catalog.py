#!/usr/bin/env python3
"""Capture catalog order from a macOS-generated optical HFS+ volume."""

import argparse
from pathlib import Path
import subprocess
import tempfile

from capture_native_names import catalog_file_names, units

VOLUME_NAME = "Collation Fixture"
NAMES = (
    "Q",
    "[",
    "_",
    "é",
    "Æ",
    "\u0254",
    "\u0181",
    "\u03a3a",
    "\u03c2b",
    "\u0416",
    "\u0531",
    "\u10a0",
    "\u10d1",
    "\u200bA",
    "\u200d$",
    "\u200eB",
    "\u202eA",
    "\u212b",
    "\u2169",
    "\u2178",
    "\uff21",
    "\U00010400",
    "\U00010429",
    "\U0001f600",
    "\ufeff&",
)


def capture():
    with tempfile.TemporaryDirectory(prefix="hfsplus-native-collation-") as directory:
        root = Path(directory)
        entries = root / "entries"
        entries.mkdir()
        for name in NAMES:
            (entries / name).touch(exist_ok=False)
        image = root / "fixture.dmg"
        subprocess.run([
            "/usr/bin/hdiutil", "makehybrid", "-hfs", "-hfs-volume-name", VOLUME_NAME,
            "-o", str(image), str(entries),
        ], check=True, stdout=subprocess.DEVNULL)
        observed = tuple(catalog_file_names(image.read_bytes()))
        if len(set(observed)) != len(NAMES) or len(observed) != len(NAMES):
            raise RuntimeError("native catalog did not contain one file per case")
        lines = ["# UTF-16 units in native optical catalog leaf order. See UNICODE32.md."]
        lines.extend(units(name) for name in observed)
        return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    args.output.write_text(capture(), encoding="utf-8")


if __name__ == "__main__":
    main()

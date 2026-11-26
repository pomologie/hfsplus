#!/usr/bin/env python3
"""Capture name operations on a disposable macOS HFS+ filesystem."""

import argparse
from contextlib import contextmanager
import errno
import os
from pathlib import Path
import plistlib
import re
import subprocess
import sys
from tempfile import TemporaryDirectory
from unicodedata import ucd_3_2_0

from capture_collation_catalog import NAMES as ORDER_NAMES
from capture_native_names import AGREEMENT_CASES, NEWER_OPTICAL_CASES, catalog_file_names, units


HDIUTIL = "/usr/bin/hdiutil"
WHOLE_DEVICE = re.compile(r"/dev/disk[0-9]+")
VOLUME = "Historical Name Witness"
MAX_UNITS = 255
IMAGE_SIZE = "64m"
TIMEOUT = 60
SUPPLEMENTARY_RANGE = range(0x10000, 0x110000)
EXCLUDED_SUPPLEMENTARY_RANGE = range(0x2F800, 0x2FB00)
SURROUNDING_MARKS = ("A\u0345", "\u0327\u0301")
FIXTURE = Path(__file__).resolve().parents[1] / "tests/fixtures/native-kernel-names.tsv"
COLLISION_PAIRS = (
    ("composed", "Café", "Cafe\u0301"),
    ("combining", "A\u0301\u0327", "A\u0327\u0301"),
    ("ignorable", "Résumé", "Ré\u200dsumé"),
    ("case", "Æ", "æ"),
    ("sigma", "Σ", "ς"),
    ("supplementary-case", "\U00010400", "\U00010428"),
    ("excluded-angstrom", "\u212b", "Å"),
)
LENGTH_CASES = (
    ("ascii-fit", "a" * MAX_UNITS),
    ("ascii-over", "a" * (MAX_UNITS + 1)),
    ("supplementary-fit", "😀" * (MAX_UNITS // 2) + "界"),
    ("supplementary-over", "😀" * (MAX_UNITS // 2 + 1)),
    ("decomposed-fit", "é" * (MAX_UNITS // 2) + "a"),
    ("decomposed-over", "é" * (MAX_UNITS // 2 + 1)),
)
POLICY_CASES = (
    ("empty", ""),
    ("dot", "."),
    ("dot-dot", ".."),
    ("nul", "nul\0suffix"),
    ("slash", "component/child"),
    ("colon", "colon:name"),
    ("nul-symbol", "symbol-\u2400"),
    ("noncharacter", "noncharacter-\uffff"),
)
PRIVATE_CASES = (
    ("private-file-namespace", "\u2400" * 4 + "HFS+ Private Data"),
    ("private-directory-namespace", ".HFS+ Private Directory Data\r"),
)


def run(*command):
    result = subprocess.run(command, capture_output=True, timeout=TIMEOUT)
    if result.returncode:
        message = (result.stdout + result.stderr).decode("utf-8", errors="replace")
        raise ValueError(f"{command[0]} returned {result.returncode}\n{message.strip()}")
    return result.stdout


@contextmanager
def attached(image, mount):
    attachment = plistlib.loads(run(
        HDIUTIL, "attach", "-plist", "-readwrite", "-nobrowse", "-noautoopen",
        "-owners", "off", "-mountpoint", mount, image,
    ))
    entries = attachment["system-entities"]
    devices = [
        entry["dev-entry"] for entry in entries
        if WHOLE_DEVICE.fullmatch(entry.get("dev-entry", ""))
    ]
    try:
        if len(devices) != 1:
            raise ValueError("expected one whole virtual device for the disposable image")
        if not any(entry.get("mount-point") == str(mount) for entry in entries):
            raise ValueError("the disposable device did not attach at its scratch mount point")
        if os.statvfs(mount).f_flag & os.ST_RDONLY:
            raise ValueError("the disposable mount is read-only")
        yield devices[0]
    finally:
        for device in devices:
            run(HDIUTIL, "detach", device)


@contextmanager
def directory_fd(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        yield descriptor
    finally:
        os.close(descriptor)


def create(directory, name):
    with directory_fd(directory) as descriptor:
        try:
            child = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600, dir_fd=descriptor)
        except OSError as error:
            return errno.errorcode[error.errno]
        except ValueError:
            return "ffi-invalid"
        os.close(child)
        return "created"


def new_file(directory, name):
    result = create(directory, name)
    if result != "created":
        raise ValueError(f"could not create an ordinary witness {name!r}: {result}")


def supplementary_cases():
    pairs = []
    marks = []
    for codepoint in SUPPLEMENTARY_RANGE:
        scalar = chr(codepoint)
        decomposition = ucd_3_2_0.decomposition(scalar)
        if decomposition and not decomposition.startswith("<") and codepoint not in EXCLUDED_SUPPLEMENTARY_RANGE:
            pairs.append((f"supplementary-{codepoint:x}", scalar, ucd_3_2_0.normalize("NFD", scalar)))
        if ucd_3_2_0.combining(scalar):
            before, after = SURROUNDING_MARKS
            marks.append(before + scalar + after)
    return pairs, marks


def populate(mount):
    names = mount / "names"
    order = mount / "order"
    policy = mount / "policy"
    for directory in (names, order, policy):
        directory.mkdir()
    supplementary, marks = supplementary_cases()
    name_inputs = (*AGREEMENT_CASES, *NEWER_OPTICAL_CASES, *(first for _, first, _ in supplementary), *marks)
    normalization_inputs = tuple(f"{index:02d}-{name}" for index, name in enumerate(name_inputs))
    for name in normalization_inputs:
        new_file(names, name)
    for name in ORDER_NAMES:
        new_file(order, name)

    rows = []
    for label, first, second in (*COLLISION_PAIRS, *supplementary):
        pair = mount / label
        pair.mkdir()
        new_file(pair, first)
        result = create(pair, second)
        same_inode = (pair / first).stat().st_ino == (pair / second).stat().st_ino
        if (result, same_inode) not in (("EEXIST", True), ("created", False)):
            raise ValueError(f"inconsistent native results for {label}")
        rows.append(("pair", label, units(first), units(second), "same" if same_inode else "distinct"))

    lengths = []
    for label, name in LENGTH_CASES:
        directory = mount / label
        directory.mkdir()
        result = create(directory, name)
        lengths.append((label, name, result, directory.stat().st_ino))

    (policy / "component").mkdir()
    for label, name in POLICY_CASES:
        rows.append(("policy", label, units(name), create(policy, name)))
    new_file(mount, "hard-link-source")
    os.link(mount / "hard-link-source", mount / "hard-link-alias")
    for label, name in PRIVATE_CASES:
        rows.append(("policy", label, units(name), create(mount, name)))
    return rows, lengths, normalization_inputs, names.stat().st_ino, order.stat().st_ino, policy.stat().st_ino


def capture():
    with TemporaryDirectory(prefix="hfsplus-kernel-names-") as temporary:
        root = Path(temporary).resolve()
        mount = root / "mount"
        mount.mkdir()
        image = root / "native.dmg"
        run(HDIUTIL, "create", "-size", IMAGE_SIZE, "-fs", "HFS+",
            "-volname", VOLUME, "-layout", "NONE", "-o", image)
        with attached(image, mount):
            rows, lengths, inputs, names_id, order_id, policy_id = populate(mount)
        run(HDIUTIL, "convert", image, "-format", "UDTO", "-o", root / "native", "-quiet")
        raw = (root / "native.cdr").read_bytes()
        observed_names = tuple(catalog_file_names(raw, volume=0, parent=names_id))
        if len(observed_names) != len(inputs):
            raise ValueError("wrong count of stored normalization witnesses")
        for original, observed in zip(inputs, observed_names, strict=True):
            if original[:3] != observed[:3]:
                raise ValueError("misaligned normalization witness")
            rows.append(("normalize", units(original), units(observed)))
        for label, original, result, parent in lengths:
            observed = tuple(catalog_file_names(raw, volume=0, parent=parent))
            if len(observed) != int(result == "created"):
                raise ValueError(f"wrong catalog count for {label}")
            rows.append(("length", label, units(original), result, *(units(name) for name in observed)))
        ordered_names = tuple(catalog_file_names(raw, volume=0, parent=order_id))
        if len(ordered_names) != len(ORDER_NAMES):
            raise ValueError("wrong count of stored catalog order witnesses")
        for name in ordered_names:
            rows.append(("order", units(name)))
        for name in catalog_file_names(raw, volume=0, parent=policy_id):
            rows.append(("policy-key", units(name)))
        return "# Native HFS+ name observations. See UNICODE32.md.\n" + "\n".join("\t".join(row) for row in rows) + "\n"


def main():
    if sys.platform != "darwin":
        raise ValueError("native HFS+ name capture requires macOS")
    parser = argparse.ArgumentParser(description=__doc__)
    destination = parser.add_mutually_exclusive_group()
    destination.add_argument("--check", action="store_true")
    destination.add_argument("--output", type=Path)
    options = parser.parse_args()
    result = capture()
    if options.check:
        if FIXTURE.read_text(encoding="utf-8") != result:
            raise ValueError("native name evidence differs from the tracked fixture")
    elif options.output is not None:
        with options.output.open("x", encoding="utf-8", newline="\n") as output:
            output.write(result)
    else:
        print(result, end="")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.TimeoutExpired) as error:
        print(error, file=sys.stderr)
        sys.exit(1)

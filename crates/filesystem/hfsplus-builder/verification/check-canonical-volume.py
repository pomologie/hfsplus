#!/usr/bin/env python3
"""Check the pinned raw HFS+ witness for H-BUILD-05-T01 and T02."""

import argparse
import hashlib
import os
from pathlib import Path
import plistlib
import re
import subprocess
import sys
from tempfile import TemporaryDirectory
import tomllib
import unicodedata


ROOT = Path(__file__).resolve().parents[4]
PACKAGE = "hfsplus-builder"
TEST = "canonical_volume"
EMITTER = "emit_canonical_volume"
OUTPUT_ENV = "POMOLOGIE_RAW_HFS_WITNESS_OUTPUT"
REVERSE_ENV = "POMOLOGIE_RAW_HFS_WITNESS_REVERSE"
IMAGE_BYTES = 8 * 1024 * 1024
EXPECTED_SHA256 = "63e2f4d50df05f59a1e6dbe63c77e5d75b10e1e2780bae068af3eec4faeadba6"
CARGO_TIMEOUT = 120
ORACLE_TIMEOUT = 30
HDIUTIL = "/usr/bin/hdiutil"
FSCK_HFS = "/sbin/fsck_hfs"
WHOLE_DEVICE = re.compile(r"/dev/disk[0-9]+")
VOLUME = "Pomologie Fixture"
RESOURCES = "Synthetic.app/Contents/Resources"
GUIDE_PATH = unicodedata.normalize("NFD", f"{RESOURCES}/Café.txt")
# p7zip 17.05 cannot select the decomposed name as a literal argument.
REFERENCE_FILES = {
	f"{RESOURCES}/Caf*.txt": b"Unicode HFS Plus witness\n",
	f"{RESOURCES}/Start": b"../MacOS/Witness",
	"[HFS+ Private Data]/iNode100": b"#!/bin/sh\nexit 0\n",
}


def run(*args, environment=None, timeout=ORACLE_TIMEOUT):
	try:
		return subprocess.run(
			args,
			cwd=ROOT,
			env=environment,
			capture_output=True,
			check=True,
			timeout=timeout,
		).stdout
	except subprocess.CalledProcessError as error:
		output = (error.stdout + error.stderr).decode("utf-8", errors="replace").strip()
		raise ValueError(f"{args[0]} exited {error.returncode}\n{output}") from error


def check_digest(path):
	if path.stat().st_size != IMAGE_BYTES:
		raise ValueError(f"{path.name} does not contain exactly {IMAGE_BYTES} bytes")
	with path.open("rb") as image:
		digest = hashlib.file_digest(image, "sha256").hexdigest()
	if digest != EXPECTED_SHA256:
		raise ValueError(f"{path.name}: SHA-256 {digest}, expected {EXPECTED_SHA256}")
	print(f"{path.name} {digest}")


def emit(path, toolchain, compiler, reverse):
	environment = os.environ.copy()
	environment["RUSTC"] = compiler
	environment[OUTPUT_ENV] = str(path)
	environment.pop(REVERSE_ENV, None)
	if reverse:
		environment[REVERSE_ENV] = "1"
	run(
		"rustup", "run", toolchain, "cargo", "test", "--locked", "--no-default-features",
		"-p", PACKAGE, "--test", TEST, "--", "--exact", "--ignored", EMITTER,
		environment=environment,
		timeout=CARGO_TIMEOUT,
	)
	check_digest(path)


def verify_reference(image, seven_zip):
	listing = run(seven_zip, "l", "-slt", "-sccUTF-8", image).decode("utf-8").splitlines()
	if f"Path = {VOLUME}/{GUIDE_PATH}" not in listing:
		raise ValueError("7-Zip did not list the HFS+-decomposed Unicode path")
	for path, expected in REFERENCE_FILES.items():
		observed = run(seven_zip, "x", "-so", "-bd", image, f"{VOLUME}/{path}")
		if observed != expected:
			raise ValueError(f"7-Zip readback differs at {path!r}")
	print("7-Zip matched the Unicode file, symlink bytes, and backing link inode data")


def verify_native(image):
	wrapped = image.parent / "witness.dmg"
	run(HDIUTIL, "convert", "-quiet", image, "-srcimagekey", "diskimage-class=CRawDiskImage",
		"-format", "UDRO", "-o", wrapped)
	attachment = plistlib.loads(run(HDIUTIL, "attach", "-readonly", "-nomount", "-plist", wrapped))
	devices = [entry["dev-entry"] for entry in attachment["system-entities"]
		if WHOLE_DEVICE.fullmatch(entry.get("dev-entry", ""))]
	try:
		if len(devices) != 1:
			raise ValueError("expected one whole device for the disposable partitionless image")
		print(run(FSCK_HFS, "-fn", devices[0]).decode("utf-8", errors="replace").strip())
	finally:
		for device in devices:
			run(HDIUTIL, "detach", device)


def main():
	parser = argparse.ArgumentParser(description=__doc__)
	parser.add_argument("--seven-zip", metavar="COMMAND", help="independent 7-Zip CLI command or path")
	parser.add_argument("--native-fsck", action="store_true", help="check the disposable image on macOS")
	options = parser.parse_args()
	if options.native_fsck and sys.platform != "darwin":
		parser.error("--native-fsck requires macOS")
	try:
		with (ROOT / "rust-toolchain.toml").open("rb") as config:
			toolchain = tomllib.load(config)["toolchain"]["channel"]
		compiler = os.fsdecode(run("rustup", "which", "--toolchain", toolchain, "rustc").strip())
		with TemporaryDirectory(prefix="hfsplus-canonical.") as temporary:
			root = Path(temporary)
			baseline = root / "canonical.hfs"
			emit(baseline, toolchain, compiler, reverse=False)
			emit(root / "reordered.hfs", toolchain, compiler, reverse=True)
			if options.seven_zip:
				verify_reference(baseline, options.seven_zip)
			if options.native_fsck:
				verify_native(baseline)
	except (OSError, KeyError, ValueError, subprocess.TimeoutExpired) as error:
		print(error, file=sys.stderr)
		return 1
	return 0


if __name__ == "__main__":
	sys.exit(main())

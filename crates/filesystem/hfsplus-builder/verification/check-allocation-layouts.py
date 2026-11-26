#!/usr/bin/env python3
"""Check disposable HFS+ allocation layouts with Apple's no-write checker."""

import os
from pathlib import Path
import plistlib
import re
import subprocess
import sys
from tempfile import TemporaryDirectory
import tomllib


ROOT = Path(__file__).resolve().parents[4]
FIXTURES = ("empty", "small", "large", "many")
INVALID_FIXTURES = {
	"overlap": "overlapped extent allocation",
	"free-counter": "invalid volume free block count",
}
HDIUTIL = "/usr/bin/hdiutil"
FSCK_HFS = "/sbin/fsck_hfs"
WHOLE_DEVICE = re.compile(r"/dev/disk[0-9]+")
NATIVE_TIMEOUT = 60
BUILD_TIMEOUT = 180


def run(*command, timeout=NATIVE_TIMEOUT, check=True, **options):
	return subprocess.run(command, cwd=ROOT, timeout=timeout, check=check, **options)


def check(image, expected_diagnostic=None):
	attachment = plistlib.loads(run(
		HDIUTIL, "attach", "-readonly", "-nomount", "-plist",
		"-imagekey", "diskimage-class=CRawDiskImage", image,
		stdout=subprocess.PIPE,
	).stdout)
	devices = [
		entry["dev-entry"] for entry in attachment["system-entities"]
		if WHOLE_DEVICE.fullmatch(entry.get("dev-entry", ""))
	]
	try:
		if len(devices) != 1:
			raise ValueError("expected one whole device for the disposable image")
		print(f"Checking {image.name} ({image.stat().st_size} bytes)", flush=True)
		result = run(FSCK_HFS, "-fn", devices[0], check=False, stdout=subprocess.PIPE,
			stderr=subprocess.STDOUT, text=True)
		print(result.stdout, end="", flush=True)
		if expected_diagnostic is None:
			result.check_returncode()
		elif result.returncode == 0 or expected_diagnostic not in result.stdout.lower():
			raise ValueError(f"{image.name} did not produce the expected diagnostic: {expected_diagnostic}")
	finally:
		for device in devices:
			run(HDIUTIL, "detach", device)


def main():
	if sys.platform != "darwin":
		raise ValueError("the native HFS+ checker requires macOS")
	with (ROOT / "rust-toolchain.toml").open("rb") as config:
		channel = tomllib.load(config)["toolchain"]["channel"]
	compiler = os.fsdecode(run("rustup", "which", "--toolchain", channel, "rustc", stdout=subprocess.PIPE).stdout.strip())
	with TemporaryDirectory(prefix="hfsplus-allocation-layouts-") as temporary:
		root = Path(temporary).resolve()
		environment = os.environ | {
			"RUSTC": compiler,
			"CARGO_TARGET_DIR": str(root / "target"),
			"POMOLOGIE_HFS_LAYOUT_WITNESSES": str(root),
		}
		run(
			"rustup", "run", channel, "cargo", "test", "--locked", "-p", "hfsplus-builder",
			"--test", "allocation_coverage", "--", "--exact", "--ignored", "emit_allocation_witnesses",
			timeout=BUILD_TIMEOUT, env=environment,
		)
		for name in FIXTURES:
			check(root / f"{name}.hfs")
		for name, diagnostic in INVALID_FIXTURES.items():
			check(root / f"{name}.hfs", diagnostic)


if __name__ == "__main__":
	try:
		main()
	except (OSError, ValueError, subprocess.SubprocessError) as error:
		print(error, file=sys.stderr)
		sys.exit(1)

#!/usr/bin/env python3
"""Compare both fully fragmented forks against an Apple read-only mount."""

from contextlib import contextmanager
import hashlib
import os
from pathlib import Path
import plistlib
import re
import subprocess
import sys
from tempfile import TemporaryDirectory
import tomllib


ROOT = Path(__file__).resolve().parents[4]
HDIUTIL = "/usr/bin/hdiutil"
FSCK_HFS = "/sbin/fsck_hfs"
WHOLE_DEVICE = re.compile(r"/dev/disk[0-9]+")
FILE = "fragmented"
FORK_RESOURCE = Path("..namedfork/rsrc")
IMAGE_SIZE = "64m"
CHUNKS = 20
CHUNK_BYTES = 256 * 1024
DATA_INITIAL = 1
RESOURCE_INITIAL = 41


def run(*args, env=None, timeout=60):
	result = subprocess.run(args, cwd=ROOT, env=env, capture_output=True, timeout=timeout)
	if result.returncode:
		output = (result.stdout + result.stderr).decode("utf-8", errors="replace").strip()
		raise ValueError(f"{args[0]} exited {result.returncode}\n{output}")
	return result.stdout


@contextmanager
def attached(image, *, readonly, mount=None):
	access = "-readonly" if readonly else "-readwrite"
	if mount is None:
		mode = ("-nomount",)
	else:
		mode = ("-nobrowse", "-noautoopen", "-owners", "off", "-mountpoint", mount)
	info = plistlib.loads(run(HDIUTIL, "attach", "-plist", access, *mode, image))
	entries = info["system-entities"]
	devices = [entry["dev-entry"] for entry in entries if WHOLE_DEVICE.fullmatch(entry.get("dev-entry", ""))]
	try:
		if len(devices) != 1:
			raise ValueError("expected one whole device for the disposable image")
		if mount is not None:
			if not any(entry.get("mount-point") == str(mount) for entry in entries):
				raise ValueError("the disposable image was not attached at its scratch mount point")
			if bool(os.statvfs(mount).f_flag & os.ST_RDONLY) != readonly:
				raise ValueError("the disposable mount has the wrong write policy")
		yield devices[0]
	finally:
		for device in devices:
			run(HDIUTIL, "detach", device)


def populate(image, mount):
	with attached(image, readonly=False, mount=mount):
		path = mount / FILE
		with path.open("xb") as data, (path / FORK_RESOURCE).open("wb") as resource:
			for index in range(CHUNKS):
				for stream, initial in ((data, DATA_INITIAL), (resource, RESOURCE_INITIAL)):
					stream.write(bytes([initial + index]) * CHUNK_BYTES)
					stream.flush()
					os.fsync(stream.fileno())


def capture_forks(image, mount, scratch):
	with attached(image, readonly=True) as device:
		print(run(FSCK_HFS, "-fn", device).decode("utf-8", errors="replace").strip())
	with attached(image, readonly=True, mount=mount):
		path = mount / FILE
		for source, output, initial in [
			(path, "data.bin", DATA_INITIAL),
			(path / FORK_RESOURCE, "resource.bin", RESOURCE_INITIAL),
		]:
			payload = source.read_bytes()
			expected = b"".join(bytes([initial + index]) * CHUNK_BYTES for index in range(CHUNKS))
			if payload != expected:
				raise ValueError(f"mounted {output} differs from the synthetic input")
			(scratch / output).write_bytes(payload)
			print(f"Mounted {output} SHA-256 {hashlib.sha256(payload).hexdigest()}")


def compare(scratch):
	with (ROOT / "rust-toolchain.toml").open("rb") as config:
		toolchain = tomllib.load(config)["toolchain"]["channel"]
	compiler = os.fsdecode(run("rustup", "which", "--toolchain", toolchain, "rustc").strip())
	environment = os.environ | {
		"POMOLOGIE_FULL_FORK_WITNESS": str(scratch),
		"CARGO_TARGET_DIR": str(scratch / "target"),
		"RUSTC": compiler,
	}
	output = run(
		"rustup", "run", toolchain, "cargo", "test", "--locked", "--no-default-features",
		"-p", "hfsplus-reader", "--test", "native_overflow", "--",
		"--exact", "--ignored", "--nocapture", "native_full_forks_match_mounted_bytes",
		env=environment,
	)
	print(output.decode("utf-8").strip())


def main():
	if sys.platform != "darwin":
		raise ValueError("the native HFS+ reader comparison requires macOS")
	with TemporaryDirectory(prefix="hfsplus-full-forks-") as temporary:
		scratch = Path(temporary).resolve()
		mount = scratch / "mount"
		mount.mkdir()
		image = scratch / "native.dmg"
		run(HDIUTIL, "create", "-size", IMAGE_SIZE, "-fs", "HFS+",
			"-volname", "Pomologie Overflow", "-layout", "NONE", "-o", image)
		populate(image, mount)
		capture_forks(image, mount, scratch)
		run(HDIUTIL, "convert", image, "-format", "UDTO", "-o", scratch / "native", "-quiet")
		with (scratch / "native.cdr").open("rb") as source:
			print(f"Native raw image SHA-256 {hashlib.file_digest(source, 'sha256').hexdigest()}")
		compare(scratch)


if __name__ == "__main__":
	try:
		main()
	except (OSError, KeyError, ValueError, subprocess.TimeoutExpired) as error:
		print(error, file=sys.stderr)
		sys.exit(1)

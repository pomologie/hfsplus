#!/usr/bin/env python3
"""Check the canonical HFS+ witness and the separate frozen-draft witness."""

import argparse
from contextlib import contextmanager
import hashlib
import os
from pathlib import Path
import plistlib
import re
import stat
import struct
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
XATTR = "/usr/bin/xattr"
VOLUME_HEADER_OFFSET = 1024
ALLOCATION_GEOMETRY_OFFSET = 40
ALLOCATION_GEOMETRY = struct.Struct(">III")
WHOLE_DEVICE = re.compile(r"/dev/disk[0-9]+")
VOLUME = "Pomologie Fixture"
MACOS_DIRECTORY = "Synthetic.app/Contents/MacOS"
RESOURCES = "Synthetic.app/Contents/Resources"
GUIDE_PATH = unicodedata.normalize("NFD", f"{RESOURCES}/Café.txt")
PRIMARY = f"{MACOS_DIRECTORY}/Witness"
ALIAS = f"{RESOURCES}/WitnessAlias"
LINK = f"{RESOURCES}/Start"
INFO = "Synthetic.app/Contents/Info.plist"
OWNER = 1234
GROUP = 567
UNIX_DATE = 1704195296
DIRECTORY_MODE = 0o755
GUIDE_DATA = b"Unicode HFS Plus witness\n"
EXECUTABLE_DATA = b"#!/bin/sh\nexit 0\n"
FORK = "com.apple.ResourceFork"
NATIVE_XATTRS = {
	(PRIMARY, FORK): b"Pomologie\0resource fork\n",
	(ALIAS, FORK): b"Pomologie\0resource fork\n",
	(PRIMARY, "org.pomologie.witness.shared"): b"shared\0attribute\xff",
	(ALIAS, "org.pomologie.witness.shared"): b"shared\0attribute\xff",
	(GUIDE_PATH, "org.pomologie.witness.guide"): b"guide metadata",
}
NATIVE_DIRECTORIES = (
	"Synthetic.app", "Synthetic.app/Contents", MACOS_DIRECTORY, RESOURCES,
)
NATIVE_FILE_MODES = {PRIMARY: 0o751, ALIAS: 0o751, INFO: 0o644, GUIDE_PATH: 0o644}
# p7zip 17.05 cannot select the decomposed name as a literal argument.
REFERENCE_FILES = {
	f"{RESOURCES}/Caf*.txt": GUIDE_DATA,
	LINK: b"../MacOS/Witness",
	"[HFS+ Private Data]/iNode100": EXECUTABLE_DATA,
}
FROZEN_DRAFT_TEST = "draft_identity"
FROZEN_DRAFT_EMITTER = "emit_frozen_draft"
FROZEN_DRAFT_OUTPUT_ENV = "POMOLOGIE_FROZEN_DRAFT_OUTPUT"
FROZEN_DRAFT_VOLUME = "Stable identities"
FROZEN_DRAFT_ROOT_ID = 2
FROZEN_DRAFT_GENERATED_IDS = {".DS_Store": 16, "A/nested/Asset": 20, "A/nested/Sibling": 22}
FROZEN_DRAFT_LINKS = ("A/Branch", "b/shared")
FROZEN_DRAFT_INODE = 30
FROZEN_DRAFT_LINK_DATA = b"shared identity"
FROZEN_DRAFT_BACKING_FILE = "[HFS+ Private Data]/iNode100"
GENERATED_WITNESS_IDS = struct.Struct(">II")


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


def image_digest(path):
	if path.stat().st_size != IMAGE_BYTES:
		raise ValueError(f"{path.name} does not contain exactly {IMAGE_BYTES} bytes")
	with path.open("rb") as image:
		return hashlib.file_digest(image, "sha256").hexdigest()


def check_digest(path):
	digest = image_digest(path)
	if digest != EXPECTED_SHA256:
		raise ValueError(f"{path.name}: SHA-256 {digest}, expected {EXPECTED_SHA256}")
	print(f"{path.name} {digest}")


def emit(path, toolchain, compiler, *, reverse, test=TEST, emitter=EMITTER, output_env=OUTPUT_ENV):
	environment = os.environ.copy()
	environment["RUSTC"] = compiler
	environment[output_env] = str(path)
	environment.pop(REVERSE_ENV, None)
	if reverse:
		environment[REVERSE_ENV] = "1"
	run(
		"rustup", "run", toolchain, "cargo", "test", "--locked", "--no-default-features",
		"-p", PACKAGE, "--test", test, "--", "--exact", "--ignored", emitter,
		environment=environment,
		timeout=CARGO_TIMEOUT,
	)


def verify_reference(image, seven_zip):
	listing = run(seven_zip, "l", "-slt", "-sccUTF-8", image).decode("utf-8").splitlines()
	if f"Path = {VOLUME}/{GUIDE_PATH}" not in listing:
		raise ValueError("7-Zip did not list the HFS+-decomposed Unicode path")
	for path, expected in REFERENCE_FILES.items():
		observed = run(seven_zip, "x", "-so", "-bd", image, f"{VOLUME}/{path}")
		if observed != expected:
			raise ValueError(f"7-Zip readback differs at {path!r}")
	print("7-Zip matched the Unicode file, symlink bytes, and backing link inode data")


def wrap_and_check(image):
	wrapped = image.with_suffix(".dmg")
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
	return wrapped


def verify_mounted_metadata(mount):
	app = mount / "Synthetic.app"
	entries = {str(path.relative_to(mount)) for path in app.rglob("*")}
	entries.add(app.name)
	expected = {*NATIVE_DIRECTORIES, *NATIVE_FILE_MODES, LINK}
	if entries != expected:
		raise ValueError(f"native app tree differs from the manifest: {entries ^ expected}")
	for relative, mode in NATIVE_FILE_MODES.items():
		entry = (mount / relative).lstat()
		if not stat.S_ISREG(entry.st_mode) or stat.S_IMODE(entry.st_mode) != mode:
			raise ValueError(f"native file type or mode differs at {relative}")
		verify_native_metadata(relative, entry)
	for relative in NATIVE_DIRECTORIES:
		entry = (mount / relative).lstat()
		if not stat.S_ISDIR(entry.st_mode) or stat.S_IMODE(entry.st_mode) != DIRECTORY_MODE:
			raise ValueError(f"native directory type or mode differs at {relative}")
		verify_native_metadata(relative, entry)
	primary, alias = ((mount / path).lstat() for path in (PRIMARY, ALIAS))
	if primary.st_ino != alias.st_ino or primary.st_nlink != 2 or alias.st_nlink != 2:
		raise ValueError("native hard-link names do not share the expected inode and link count")
	link = (mount / LINK).lstat()
	if not stat.S_ISLNK(link.st_mode):
		raise ValueError("native symbolic link is absent from the manifest path")
	verify_native_metadata(LINK, link)
	print("macOS matched the app tree, ownership, timestamps, modes, and hard-link identity")


def verify_mounted_payloads(mount):
	if os.readlink(mount / LINK).encode() != REFERENCE_FILES[LINK]:
		raise ValueError("native symbolic-link bytes differ from the manifest")
	if (mount / GUIDE_PATH).read_bytes() != GUIDE_DATA:
		raise ValueError("native Unicode file data differs from the manifest")
	for path in (PRIMARY, ALIAS):
		if (mount / path).read_bytes() != EXECUTABLE_DATA:
			raise ValueError(f"native executable data differs at {path}")
	info = plistlib.loads((mount / INFO).read_bytes())
	if info != {
		"CFBundleExecutable": "Witness",
		"CFBundleIdentifier": "org.pomologie.hfsplus.witness",
		"CFBundlePackageType": "APPL",
	}:
		raise ValueError("native bundle Info.plist differs from the manifest")
	for (path, name), expected in NATIVE_XATTRS.items():
		observed = bytes.fromhex(run(XATTR, "-px", name, mount / path).decode("ascii"))
		if observed != expected:
			raise ValueError(f"native attribute {name} differs at {path}")
	print("macOS matched data and resource forks, extended attributes, and symbolic-link bytes")


def verify_native_metadata(path, entry):
	if (entry.st_uid, entry.st_gid, entry.st_mtime, entry.st_birthtime) != (OWNER, GROUP, UNIX_DATE, UNIX_DATE):
		raise ValueError(f"native ownership or timestamps differ at {path}")


def verify_mounted_image(image, wrapped):
	with image.open("rb") as stream:
		stream.seek(VOLUME_HEADER_OFFSET + ALLOCATION_GEOMETRY_OFFSET)
		block_bytes, total_blocks, free_blocks = ALLOCATION_GEOMETRY.unpack(
			stream.read(ALLOCATION_GEOMETRY.size),
		)
	if block_bytes * total_blocks != IMAGE_BYTES or free_blocks > total_blocks:
		raise ValueError("the canonical volume header has unexpected allocation geometry")

	mount = image.parent / "mount"
	mount.mkdir()
	with mounted_read_only(wrapped, mount, owners="on") as mounted:
		if (mounted.f_blocks * mounted.f_frsize, mounted.f_bfree * mounted.f_frsize) != (
			total_blocks * block_bytes, free_blocks * block_bytes,
		):
			raise ValueError("native total or free bytes differ from the volume header")
		print(f"macOS matched {total_blocks} total and {free_blocks} free {block_bytes}-byte allocation blocks")
		verify_mounted_metadata(mount)
	# The witness includes execute-only permissions for non-owners. Ignore
	# ownership on a second read-only attachment to examine those payloads.
	with mounted_read_only(wrapped, mount, owners="off"):
		verify_mounted_payloads(mount)


@contextmanager
def mounted_read_only(wrapped, mount, owners):
	attachment = plistlib.loads(run(
		HDIUTIL, "attach", "-readonly", "-nobrowse", "-noautoopen", "-owners", owners,
		"-mountpoint", mount, "-plist", wrapped,
	))
	entries = attachment["system-entities"]
	devices = [entry["dev-entry"] for entry in entries if WHOLE_DEVICE.fullmatch(entry.get("dev-entry", ""))]
	try:
		if len(devices) != 1 or not any(entry.get("mount-point") == str(mount) for entry in entries):
			raise ValueError("expected the disposable partitionless image at the requested mount point")
		mounted = os.statvfs(mount)
		if not mounted.f_flag & os.ST_RDONLY:
			raise ValueError("expected a read-only mounted volume")
		yield mounted
	finally:
		for device in devices:
			run(HDIUTIL, "detach", device)


def verify_frozen_draft(root, toolchain, compiler, options):
	baseline = root / "frozen-draft.hfs"
	reordered = root / "frozen-draft-reordered.hfs"
	for path, reverse in ((baseline, False), (reordered, True)):
		emit(path, toolchain, compiler, reverse=reverse, test=FROZEN_DRAFT_TEST,
			emitter=FROZEN_DRAFT_EMITTER, output_env=FROZEN_DRAFT_OUTPUT_ENV)
	digest = image_digest(baseline)
	if image_digest(reordered) != digest:
		raise ValueError("the frozen draft changed after permuting manifest and binding order")
	print(f"Frozen draft and reordered image SHA-256 {digest}")
	generated = {
		path: GENERATED_WITNESS_IDS.pack(cnid, FROZEN_DRAFT_INODE) + bytes([ordinal])
		for ordinal, (path, cnid) in enumerate(FROZEN_DRAFT_GENERATED_IDS.items(), start=1)
	}
	if options.seven_zip:
		files = generated | {FROZEN_DRAFT_BACKING_FILE: FROZEN_DRAFT_LINK_DATA}
		for relative, expected in files.items():
			observed = run(options.seven_zip, "x", "-so", "-bd", baseline, f"{FROZEN_DRAFT_VOLUME}/{relative}")
			if observed != expected:
				raise ValueError(f"7-Zip frozen-draft readback differs at {relative!r}")
		print("7-Zip matched all three generated ID payloads and the shared backing inode data")
	if options.native_fsck:
		wrapped = wrap_and_check(baseline)
		mount = root / "frozen-draft-mount"
		mount.mkdir()
		with mounted_read_only(wrapped, mount, owners="off"):
			verify_frozen_draft_mount(mount, generated)


def verify_frozen_draft_mount(mount, generated):
	if mount.stat().st_ino != FROZEN_DRAFT_ROOT_ID:
		raise ValueError("native frozen-draft root ID did not match the reserved catalog ID")
	for relative, cnid in FROZEN_DRAFT_GENERATED_IDS.items():
		path = mount / relative
		if path.stat().st_ino != cnid or path.read_bytes() != generated[relative]:
			raise ValueError(f"native generated asset ID or payload differs at {relative!r}")
	for relative in FROZEN_DRAFT_LINKS:
		path = mount / relative
		entry = path.stat()
		if (entry.st_ino, entry.st_nlink) != (FROZEN_DRAFT_INODE, len(FROZEN_DRAFT_LINKS)):
			raise ValueError(f"native hard-link identity or count differs at {relative!r}")
		if path.read_bytes() != FROZEN_DRAFT_LINK_DATA:
			raise ValueError(f"native hard-link data differs at {relative!r}")
	print(f"macOS matched root CNID {FROZEN_DRAFT_ROOT_ID}, generated CNIDs "
		f"{tuple(FROZEN_DRAFT_GENERATED_IDS.values())}, and shared inode {FROZEN_DRAFT_INODE} with two links")


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
			root = Path(temporary).resolve()
			baseline = root / "canonical.hfs"
			for path, reverse in ((baseline, False), (root / "reordered.hfs", True)):
				emit(path, toolchain, compiler, reverse=reverse)
				check_digest(path)
			if options.seven_zip:
				verify_reference(baseline, options.seven_zip)
			if options.native_fsck:
				verify_mounted_image(baseline, wrap_and_check(baseline))
			if options.seven_zip or options.native_fsck:
				verify_frozen_draft(root, toolchain, compiler, options)
	except (OSError, KeyError, ValueError, subprocess.TimeoutExpired) as error:
		print(error, file=sys.stderr)
		return 1
	return 0


if __name__ == "__main__":
	sys.exit(main())

#!/usr/bin/env python3
"""Compare the valid frozen-draft witness with independent HFS+ wire facts."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import stat
import struct
import subprocess
import sys
from tempfile import TemporaryDirectory
import tomllib

from interleaved_fork_oracle import (
	BLOCK_BYTES, BLOCK_BYTES_OFFSET, CATALOG_FORK_OFFSET, CATALOG_KEY, FILE_FORKS,
	FILE_RECORD, HEADER_OFFSET, RECORD_OFFSET, contiguous_metadata, fork, leaf_records,
)

from readonly_mount import mount_argv, mounted_read_only


ROOT = Path(__file__).resolve().parents[4]
IMAGE_BYTES = 8 * 1024 * 1024
IMAGE_SHA256 = "ad8f9af8d574eedf989b154ab5fca2d9a57d50c4b9fc692c90a21114f983d0bb"
EMIT_OUTPUT = "POMOLOGIE_FROZEN_DRAFT_OUTPUT"
EMIT_REVERSE = "POMOLOGIE_RAW_HFS_WITNESS_REVERSE"
EMIT_TIMEOUT_SECONDS = 120
NATIVE_TIMEOUT_SECONDS = 30
SW_VERS = "/usr/bin/sw_vers"
FOLDER_RECORD = 1
THREAD_KIND = {FOLDER_RECORD: 3, FILE_RECORD: 4}
THREAD_FIELDS = struct.Struct(">HHIH")
UINT32 = struct.Struct(">I")
OBJECT_ID_OFFSET = 8
NEXT_ID_OFFSET = 64
BSD_SPECIAL_OFFSET = 44
FINDER_CODE_OFFSET = 48
LINK_FINDER_CODE = b"hlnkhfs+"
GENERATED_IDS = struct.Struct(">II")
GENERATED_RESOURCE = b"resource"
ROOT_PARENT = 1
ROOT_ID = 2
PRIVATE_DIR = 29
SHARED_INODE = 30
LINK_REFERENCE = 100
NEXT_ID = 31
SHARED_PAYLOAD = b"shared identity"
LONG_NAME = "LongerName" * 20

# Pinned reference for the admitted manifest, including the reserved namespace.
# A tuple records the parent, exact stored spelling and forward record kind.
OBJECTS = {
	ROOT_ID: (ROOT_PARENT, "Stable identities", FOLDER_RECORD),
	16: (ROOT_ID, ".DS_Store", FILE_RECORD),
	17: (ROOT_ID, "A", FOLDER_RECORD),
	18: (17, "Branch", FILE_RECORD),
	19: (17, "nested", FOLDER_RECORD),
	20: (19, "Asset", FILE_RECORD),
	21: (19, LONG_NAME, FILE_RECORD),
	22: (19, "Sibling", FILE_RECORD),
	23: (ROOT_ID, "a-keep", FILE_RECORD),
	24: (ROOT_ID, "b", FOLDER_RECORD),
	25: (24, "shared", FILE_RECORD),
	26: (24, "sibling", FILE_RECORD),
	27: (24, "Target", FILE_RECORD),
	28: (ROOT_ID, "c", FILE_RECORD),
	PRIVATE_DIR: (ROOT_ID, "\0" * 4 + "HFS+ Private Data", FOLDER_RECORD),
	SHARED_INODE: (PRIVATE_DIR, f"iNode{LINK_REFERENCE}", FILE_RECORD),
}
GENERATED = (16, 20, 22)
ALIASES = (18, 25)


def number(data, offset):
	return UINT32.unpack_from(data, offset)[0]


def read_fork(image, record, offset):
	length, total, extents = fork(record, offset)
	if not length and not total and not extents:
		return b""
	if len(extents) != 1 or extents[0][1] != total or length > total * BLOCK_BYTES:
		raise ValueError("unexpected allocation for a contiguous frozen-draft fork")
	start = extents[0][0] * BLOCK_BYTES
	if start + total * BLOCK_BYTES > len(image):
		raise ValueError("frozen-draft fork exceeds the volume")
	return image[start:start + length]


def catalog_records(image):
	depth, records = leaf_records(contiguous_metadata(image, CATALOG_FORK_OFFSET))
	objects, threads, files, keys = {}, {}, {}, set()
	for raw in records:
		width, parent, units = CATALOG_KEY.unpack_from(raw)
		end = RECORD_OFFSET.size + width
		name = raw[CATALOG_KEY.size:end].decode("utf-16-be")
		if end != CATALOG_KEY.size + units * 2 or (parent, name) in keys:
			raise ValueError("incorrect or repeated frozen-draft catalog key")
		keys.add((parent, name))
		data = raw[end:]
		kind = RECORD_OFFSET.unpack_from(data)[0]
		if kind in THREAD_KIND:
			cnid = number(data, OBJECT_ID_OFFSET)
			if cnid in objects:
				raise ValueError("repeated frozen-draft object ID")
			objects[cnid] = (parent, name, kind)
			if kind == FILE_RECORD:
				files[cnid] = data
		elif kind in THREAD_KIND.values():
			_, reserved, target, length = THREAD_FIELDS.unpack_from(data)
			if name or reserved or parent in threads or len(data) != THREAD_FIELDS.size + length * 2:
				raise ValueError("incorrect or repeated frozen-draft thread")
			threads[parent] = target, data[THREAD_FIELDS.size:].decode("utf-16-be"), kind
		else:
			raise ValueError("unexpected frozen-draft record kind")
	return depth, objects, threads, files


def verify_image(image):
	if number(image, HEADER_OFFSET + BLOCK_BYTES_OFFSET) != BLOCK_BYTES:
		raise ValueError("unexpected frozen-draft allocation block size")
	if number(image, HEADER_OFFSET + NEXT_ID_OFFSET) != NEXT_ID:
		raise ValueError("unexpected next frozen-draft catalog ID")
	depth, objects, threads, files = catalog_records(image)
	expected_threads = {cnid: (parent, name, THREAD_KIND[kind])
		for cnid, (parent, name, kind) in OBJECTS.items()}
	if objects != OBJECTS or threads != expected_threads:
		raise ValueError("frozen-draft objects or threads differ from the reference catalog")
	data_offset, resource_offset = FILE_FORKS
	for ordinal, cnid in enumerate(GENERATED, start=1):
		expected = GENERATED_IDS.pack(cnid, SHARED_INODE) + bytes([ordinal])
		if (read_fork(image, files[cnid], data_offset) != expected
			or read_fork(image, files[cnid], resource_offset) != GENERATED_RESOURCE):
			raise ValueError(f"generated payload or resource differs for CNID {cnid}")
	for cnid in ALIASES:
		record = files[cnid]
		if (record[FINDER_CODE_OFFSET:FINDER_CODE_OFFSET + len(LINK_FINDER_CODE)] != LINK_FINDER_CODE
			or number(record, BSD_SPECIAL_OFFSET) != LINK_REFERENCE
			or any(read_fork(image, record, offset) for offset in FILE_FORKS)):
			raise ValueError(f"incorrect hard-link stub for CNID {cnid}")
	inode = files[SHARED_INODE]
	if (number(inode, BSD_SPECIAL_OFFSET) != len(ALIASES)
		or read_fork(image, inode, data_offset) != SHARED_PAYLOAD):
		raise ValueError("incorrect shared inode link count or payload")
	print(f"Independent catalog matched {len(objects)} objects and {len(threads)} threads "
		f"at depth {depth}, with root CNID {ROOT_ID} and next CNID {NEXT_ID}")
	print(f"Independent forks matched generated CNIDs {GENERATED}, "
		f"hard-link stubs {ALIASES}, and shared inode {SHARED_INODE} with reference {LINK_REFERENCE}")


def run(toolchain, *args, **kwargs):
	return subprocess.run(
		["rustup", "run", toolchain, *args], cwd=ROOT, check=True, capture_output=True,
		text=True, timeout=EMIT_TIMEOUT_SECONDS, **kwargs,
	).stdout.strip()


def run_native(*argv):
	return subprocess.run(argv, check=True, capture_output=True, timeout=NATIVE_TIMEOUT_SECONDS).stdout


def object_path(root, cnid):
	if cnid == ROOT_ID:
		return root
	parent, name, _ = OBJECTS[cnid]
	return object_path(root, parent) / name


def verify_native_mount(image_path, digest):
	if not stat.S_ISREG(image_path.lstat().st_mode) or digest != IMAGE_SHA256:
		raise ValueError("native identity check requires the pinned regular fixture")
	version, build = (run_native(SW_VERS, option).decode().strip()
		for option in ("-productVersion", "-buildVersion"))
	print(f"macOS {version} build {build}")
	mount = image_path.parent / "mount"
	mount.mkdir()
	argv = mount_argv("<fixture>", "<mount>", "off", raw_image=True)
	print(f"Native identity attachment argv {json.dumps(argv)}")
	with mounted_read_only(run_native, image_path, mount, owners="off", raw_image=True):
		print("Native identity mount verified read-only")
		root = mount.stat()
		if not stat.S_ISDIR(root.st_mode) or root.st_ino != ROOT_ID:
			raise ValueError("macOS did not report the root directory CNID")
		observed_generated = []
		for ordinal, cnid in enumerate(GENERATED, start=1):
			path = object_path(mount, cnid)
			entry = path.lstat()
			expected = GENERATED_IDS.pack(cnid, SHARED_INODE) + bytes([ordinal])
			if (not stat.S_ISREG(entry.st_mode) or entry.st_ino != cnid or path.read_bytes() != expected
				or (path / "..namedfork" / "rsrc").read_bytes() != GENERATED_RESOURCE):
				raise ValueError(f"macOS generated file identity or fork differs for CNID {cnid}")
			observed_generated.append(entry.st_ino)
		observed_inode = None
		for cnid in ALIASES:
			path = object_path(mount, cnid)
			entry = path.lstat()
			if (not stat.S_ISREG(entry.st_mode) or (entry.st_ino, entry.st_nlink) != (SHARED_INODE, len(ALIASES))
				or path.read_bytes() != SHARED_PAYLOAD):
				raise ValueError(f"macOS alias identity or data differs for stub CNID {cnid}")
			observed_inode = entry
		print(f"macOS matched root CNID {root.st_ino}, generated CNIDs {tuple(observed_generated)} and both forks, "
			f"and shared inode {observed_inode.st_ino} with {observed_inode.st_nlink} links and exact data")
	print("Native identity attachment detached")
	with image_path.open("rb") as source:
		observed_digest = hashlib.file_digest(source, "sha256").hexdigest()
	if observed_digest != digest:
		raise ValueError("the native identity check altered the frozen-draft image")
	print(f"Native identity image SHA-256 {observed_digest}")


def main():
	parser = argparse.ArgumentParser(description=__doc__)
	parser.add_argument("--native", action="store_true", help="check the fixture using a read-only macOS software mount")
	options = parser.parse_args()
	if options.native and sys.platform != "darwin":
		parser.error("native identity verification requires macOS")
	try:
		with (ROOT / "rust-toolchain.toml").open("rb") as config:
			toolchain = tomllib.load(config)["toolchain"]["channel"]
		print(f"Python {sys.version.split(maxsplit=1)[0]}")
		print(run(toolchain, "cargo", "--version"))
		print(run(toolchain, "rustc", "--version"))
		with TemporaryDirectory(prefix="hfsplus-frozen-identity.") as temporary:
			path = Path(temporary).resolve() / "frozen-draft.hfs"
			environment = os.environ | {EMIT_OUTPUT: str(path)}
			environment.pop(EMIT_REVERSE, None)
			run(toolchain, "cargo", "test", "--color", "never", "--locked", "--no-default-features",
				"-p", "hfsplus-builder", "--test", "draft_identity", "--", "--exact", "--ignored",
				"emit_frozen_draft", env=environment)
			if path.stat().st_size != IMAGE_BYTES:
				raise ValueError("unexpected frozen-draft fixture size")
			image = path.read_bytes()
			digest = hashlib.sha256(image).hexdigest()
			if digest != IMAGE_SHA256:
				raise ValueError(f"unexpected frozen-draft SHA-256 {digest}")
			print(f"Frozen draft image {len(image)} bytes SHA-256 {digest}")
			verify_image(image)
			if options.native:
				verify_native_mount(path, digest)
	except (OSError, ValueError, struct.error, subprocess.SubprocessError) as error:
		print(error, file=sys.stderr)
		if isinstance(error, subprocess.CalledProcessError):
			print(error.stderr, file=sys.stderr)
		return 1
	return 0


if __name__ == "__main__":
	sys.exit(main())

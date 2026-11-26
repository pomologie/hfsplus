#!/usr/bin/env python3
"""Compare a fragmented HFS+ file or nested hard-link tree with an Apple mount."""

import argparse
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
import unicodedata


ROOT = Path(__file__).resolve().parents[4]
HDIUTIL = "/usr/bin/hdiutil"
FSCK_HFS = "/sbin/fsck_hfs"
XATTR = "/usr/bin/xattr"
WHOLE_DEVICE = re.compile(r"/dev/disk[0-9]+")
FILE = "fragmented"
FORK_RESOURCE = Path("..namedfork/rsrc")
IMAGE_SIZE = "64m"
CHUNKS = 20
CHUNK_BYTES = 256 * 1024
DATA_INITIAL = 1
RESOURCE_INITIAL = 41
TREE_ROOT = Path("Résumé")
DOCUMENTS = TREE_ROOT / "Documents"
TREE_FILE = DOCUMENTS / "Fragmenté"
ALIAS = TREE_ROOT / "Alias"
SYMLINK = DOCUMENTS / "Unresolved"
SYMLINK_TARGET = b"../missing/target"
SHARED_ATTRIBUTE = "org.pomologie.fragment.shared"
FOLDER_ATTRIBUTE = "org.pomologie.fragment.folder"
SHARED_VALUE = b"shared\0attribute\xff"
FOLDER_VALUE = b"nested folder metadata"
NANOSECONDS_PER_SECOND = 1_000_000_000


def run(*args, env=None, timeout=60):
	result = subprocess.run(args, cwd=ROOT, env=env, capture_output=True, timeout=timeout)
	if result.returncode:
		output = (result.stdout + result.stderr).decode("utf-8", errors="replace").strip()
		raise ValueError(f"{args[0]} exited {result.returncode}\n{output}")
	return result.stdout


@contextmanager
def attached(image, *, readonly, mount=None, owners="off"):
	access = "-readonly" if readonly else "-readwrite"
	if mount is None:
		mode = ("-nomount",)
	else:
		mode = ("-nobrowse", "-noautoopen", "-owners", owners, "-mountpoint", mount)
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


def populate(image, mount, *, tree):
	with attached(image, readonly=False, mount=mount):
		path = mount / (TREE_FILE if tree else FILE)
		if tree:
			path.parent.mkdir(parents=True)
		with path.open("xb") as data, (path / FORK_RESOURCE).open("wb") as resource:
			for index in range(CHUNKS):
				for stream, initial in ((data, DATA_INITIAL), (resource, RESOURCE_INITIAL)):
					stream.write(bytes([initial + index]) * CHUNK_BYTES)
					stream.flush()
					os.fsync(stream.fileno())
		if tree:
			path.chmod(0o644)
			os.link(path, mount / ALIAS)
			os.symlink(os.fsdecode(SYMLINK_TARGET), mount / SYMLINK)
			run(XATTR, "-wx", SHARED_ATTRIBUTE, SHARED_VALUE.hex(), path)
			run(XATTR, "-wx", FOLDER_ATTRIBUTE, FOLDER_VALUE.hex(), mount / DOCUMENTS)


def capture_forks(image, mount, scratch, *, tree):
	with attached(image, readonly=True) as device:
		print(run(FSCK_HFS, "-fn", device).decode("utf-8", errors="replace").strip())
	with attached(image, readonly=True, mount=mount, owners="on" if tree else "off"):
		path = mount / (TREE_FILE if tree else FILE)
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
		if tree:
			capture_tree(mount, scratch)


def capture_tree(mount, scratch):
	expected = {unicodedata.normalize("NFD", str(path)) for path in (TREE_ROOT, DOCUMENTS, TREE_FILE, ALIAS, SYMLINK)}
	root = mount / unicodedata.normalize("NFD", str(TREE_ROOT))
	entries = sorted([root, *root.rglob("*")])
	if {entry.relative_to(mount).as_posix() for entry in entries} != expected:
		raise ValueError("the mounted native tree differs from its manifest")
	with (scratch / "tree.tsv").open("w", encoding="utf-8") as ledger:
		for entry in entries:
			metadata = entry.lstat()
			row = (entry.relative_to(mount), metadata.st_ino, metadata.st_mode, metadata.st_nlink,
				metadata.st_uid, metadata.st_gid, metadata.st_mtime_ns // NANOSECONDS_PER_SECOND)
			ledger.write("\t".join(map(str, row)) + "\n")
	primary, alias = (mount / TREE_FILE).stat(), (mount / ALIAS).stat()
	if primary.st_ino != alias.st_ino or primary.st_nlink != 2 or alias.st_nlink != 2:
		raise ValueError("the two mounted names do not share one native inode")
	for path, attribute, value, output in (
		(TREE_FILE, SHARED_ATTRIBUTE, SHARED_VALUE, "shared.attr"),
		(ALIAS, SHARED_ATTRIBUTE, SHARED_VALUE, "shared.attr"),
		(DOCUMENTS, FOLDER_ATTRIBUTE, FOLDER_VALUE, "folder.attr"),
	):
		observed = bytes.fromhex(run(XATTR, "-px", attribute, mount / path).decode("ascii"))
		if observed != value:
			raise ValueError(f"mounted attribute {attribute} differs at {path}")
		(scratch / output).write_bytes(observed)
	target = os.fsencode(os.readlink(mount / SYMLINK))
	if target != SYMLINK_TARGET:
		raise ValueError("the mounted symbolic-link target differs from its literal input")
	(scratch / "symlink.bin").write_bytes(target)
	print(f"Native mount matched {len(entries)} stored names, shared inode {primary.st_ino}, and exact attributes and link target")


def compare(scratch, *, tree):
	with (ROOT / "rust-toolchain.toml").open("rb") as config:
		toolchain = tomllib.load(config)["toolchain"]["channel"]
	compiler = os.fsdecode(run("rustup", "which", "--toolchain", toolchain, "rustc").strip())
	environment = os.environ | {
		"POMOLOGIE_FULL_FORK_WITNESS": str(scratch),
		"CARGO_TARGET_DIR": str(scratch / "target"),
		"RUSTC": compiler,
	}
	test = "tree::native_nested_tree_matches_mounted_entries" if tree else "native_full_forks_match_mounted_bytes"
	output = run(
		"rustup", "run", toolchain, "cargo", "test", "--locked", "--no-default-features",
		"-p", "hfsplus-reader", "--test", "native_overflow", "--",
		"--exact", "--ignored", "--nocapture", test,
		env=environment,
	)
	print(output.decode("utf-8").strip())


def main():
	parser = argparse.ArgumentParser(description=__doc__)
	parser.add_argument("--tree", action="store_true", help="include nested paths, hard links, attributes and a symlink")
	options = parser.parse_args()
	if sys.platform != "darwin":
		raise ValueError("the native HFS+ reader comparison requires macOS")
	with TemporaryDirectory(prefix="hfsplus-full-forks-") as temporary:
		scratch = Path(temporary).resolve()
		mount = scratch / "mount"
		mount.mkdir()
		image = scratch / "native.dmg"
		run(HDIUTIL, "create", "-size", IMAGE_SIZE, "-fs", "HFS+",
			"-volname", "Pomologie Overflow", "-layout", "NONE", "-o", image)
		populate(image, mount, tree=options.tree)
		capture_forks(image, mount, scratch, tree=options.tree)
		run(HDIUTIL, "convert", image, "-format", "UDTO", "-o", scratch / "native", "-quiet")
		with (scratch / "native.cdr").open("rb") as source:
			print(f"Native raw image SHA-256 {hashlib.file_digest(source, 'sha256').hexdigest()}")
		compare(scratch, tree=options.tree)


if __name__ == "__main__":
	try:
		main()
	except (OSError, KeyError, ValueError, subprocess.TimeoutExpired) as error:
		print(error, file=sys.stderr)
		sys.exit(1)

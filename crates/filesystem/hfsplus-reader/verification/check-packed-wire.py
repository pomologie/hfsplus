#!/usr/bin/env python3
"""Compare retained Apple B-tree bytes with production Rust observations."""

from hashlib import sha256
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
from tempfile import TemporaryDirectory


PACKAGE_ROOT = Path(__file__).resolve().parents[1]
FIXTURES = PACKAGE_ROOT / "tests/fixtures"
LEDGERS = ("native-catalog-volume.json", "native-overflow.json", "native-attribute-tree.json")
DESCRIPTOR = struct.Struct(">IIbBHH")
TREE_WIDTH_OFFSET = DESCRIPTOR.size + struct.calcsize(">HIIII")
VOLUME_HEADER_START = 1024
VOLUME_BLOCK_SIZE = 40
VOLUME_CATALOG_FORK = 272
FORK_HEADER = struct.Struct(">QII")
FORK_BYTES = 80
EXTENT = struct.Struct(">II")
OFFSET = struct.Struct(">H")
KIND = {-1: "leaf", 0: "index"}
TEST = "packed_wire::reports_packed_native_records_for_the_wire_oracle"


def fixture(name, digest):
	raw = (FIXTURES / name).read_bytes()
	if sha256(raw).hexdigest() != digest:
		raise ValueError(f"fixture digest mismatch for {name}")
	return raw


def uint16(raw, offset):
	return OFFSET.unpack_from(raw, offset)[0]


def uint32(raw, offset):
	return struct.unpack_from(">I", raw, offset)[0]


def units(raw, offset, count):
	return list(struct.unpack_from(f">{count}H", raw, offset))


def extent_words(descriptors):
	return [word for start, blocks in EXTENT.iter_unpack(descriptors) if blocks
		for word in (start, blocks)]


def fork(raw, offset):
	return [*FORK_HEADER.unpack_from(raw, offset),
		*extent_words(raw[offset + FORK_HEADER.size:offset + FORK_BYTES])]


def metadata(body):
	flags = uint16(body, 2)
	catalog_id, *dates = struct.unpack_from(">6I", body, 8)
	uid, gid, admin, owner, mode, special = struct.unpack_from(">IIBBHI", body, 32)
	finder = list(body[48:80])
	text_encoding = uint32(body, 80)
	return [catalog_id, flags, dates, uid, gid, admin, owner, mode, special, finder, text_encoding]


def catalog_record(kind, raw):
	owner, count = struct.unpack_from(">IH", raw, OFFSET.size)
	body = raw[OFFSET.size + uint16(raw, 0):]
	key = [owner, units(raw, 8, count)]
	if kind == "index":
		return [*key, "child", uint32(body, 0)]
	match uint16(body, 0):
		case 1:
			return [*key, "folder", *metadata(body), uint32(body, 4), uint32(body, 84)]
		case 2:
			return [*key, "file", *metadata(body), fork(body, 88), fork(body, 168)]
		case 3 | 4 as tag:
			thread = "folder-thread" if tag == 3 else "file-thread"
			return [*key, thread, uint32(body, 4), units(body, 10, uint16(body, 8))]
	raise ValueError("unexpected catalog value")


def extent_record(kind, raw):
	if kind != "leaf":
		raise ValueError("expected an extents leaf")
	fork_kind, owner, start = struct.unpack_from(">BxII", raw, OFFSET.size)
	return [owner, fork_kind, start, extent_words(raw[12:])]


def attribute_record(kind, raw):
	owner, start, count = struct.unpack_from(">IIH", raw, 4)
	body = raw[OFFSET.size + uint16(raw, 0):]
	key = [owner, units(raw, 14, count), start]
	if kind == "index":
		return [*key, "child", uint32(body, 0)]
	match uint32(body, 0):
		case 0x10:
			length = uint32(body, 12)
			return [*key, "inline", list(body[16:16 + length])]
		case 0x20:
			return [*key, "fork", fork(body, 8)]
	raise ValueError("unexpected attribute value")


def node_rows(label, node_id, node, decode_record):
	forward, backward, wire_kind, height, count, _ = DESCRIPTOR.unpack_from(node)
	kind = KIND[wire_kind]
	yield f"HFS+ node {label}|{node_id}|{len(node)}|{kind}|{height}|{forward}|{backward}|{count}"
	offsets = [uint16(node, len(node) - OFFSET.size * (index + 1)) for index in range(count + 1)]
	for ordinal, (start, end) in enumerate(zip(offsets, offsets[1:])):
		words = decode_record(kind, node[start:end])
		yield f"HFS+ record {label}|{node_id}|{ordinal}|{start}|{end}|" + "|".join(map(str, words))


def tree_rows(label, tree, decode_record):
	width = uint16(tree, TREE_WIDTH_OFFSET)
	if width not in {4096, 8192} or len(tree) % width:
		raise ValueError(f"unexpected retained {label} tree width")
	for start in range(width, len(tree), width):
		yield from node_rows(label, start // width, tree[start:start + width], decode_record)


def catalog_tree(ledger):
	raw = fixture("native-catalog-volume.bin", ledger["bounded_volume_sha256"])
	block_size = uint32(raw, VOLUME_HEADER_START + VOLUME_BLOCK_SIZE)
	descriptor = VOLUME_HEADER_START + VOLUME_CATALOG_FORK
	length, _, blocks = FORK_HEADER.unpack_from(raw, descriptor)
	start, embedded = EXTENT.unpack_from(raw, descriptor + FORK_HEADER.size)
	if embedded != blocks:
		raise ValueError("expected the retained contiguous catalog fork")
	offset = start * block_size
	return raw[offset:offset + length]


def extent_leaf(ledger):
	raw = fixture("native-overflow.bin", ledger["fixture_sha256"])
	size, count = struct.unpack_from(">QH", raw)
	if size != ledger["raw_image_bytes"] or count != len(ledger["slices"]):
		raise ValueError("unexpected retained extent fixture envelope")
	offset = struct.calcsize(">QH")
	for captured in ledger["slices"]:
		start, length = struct.unpack_from(">QI", raw, offset)
		offset += struct.calcsize(">QI")
		if (start, length) != (captured["volume_offset"], captured["bytes"]):
			raise ValueError("retained extent fixture differs from its ledger")
		if captured["purpose"] == "overflow leaf node":
			return raw[offset:offset + length]
		offset += length
	raise ValueError("missing retained extent overflow leaf")


def attribute_tree(ledger):
	raw = fixture(ledger["fixture"]["file"], ledger["fixture"]["sha256"])
	segment = ledger["sparse_segments"][-1]
	start = segment["fixture_offset"]
	return raw[start:start + segment["length"]]


def expected_rows(ledgers):
	catalog, extents, attributes = map(json.loads, ledgers)
	return [
		*tree_rows("catalog", catalog_tree(catalog), catalog_record),
		*node_rows("extents", 1, extent_leaf(extents), extent_record),
		*tree_rows("attributes", attribute_tree(attributes), attribute_record),
	]


def rust_rows():
	active = subprocess.check_output(
		["rustup", "show", "active-toolchain"], cwd=PACKAGE_ROOT, text=True,
	)
	toolchain = active.split(maxsplit=1)[0]
	cargo = ["rustup", "run", toolchain, "cargo"]
	workspace_manifest = subprocess.check_output(
		[*cargo, "locate-project", "--workspace", "--message-format", "plain"],
		cwd=PACKAGE_ROOT, text=True,
	)
	workspace = Path(workspace_manifest.strip()).parent
	compiler = subprocess.check_output(
		["rustup", "which", "--toolchain", toolchain, "rustc"], cwd=workspace, text=True,
	).strip()
	print(f"Python {sys.version.split()[0]}", flush=True)
	subprocess.run([*cargo, "--version"], cwd=workspace, check=True)
	subprocess.run([compiler, "--version"], cwd=workspace, check=True)
	with TemporaryDirectory(prefix="hfsplus-packed-wire-") as target:
		environment = os.environ | {"CARGO_TARGET_DIR": target, "RUSTC": compiler}
		run = subprocess.run(
			[*cargo, "test", "--color", "never", "--locked", "--no-default-features",
			 "-p", "hfsplus-reader", "--test", "native_overflow",
			 "--", "--exact", "--show-output", TEST],
			env=environment, cwd=workspace, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
			check=False,
		)
	if run.returncode:
		raise ValueError(run.stdout)
	return [row for row in run.stdout.splitlines() if row.startswith(("HFS+ node ", "HFS+ record "))]


def main():
	ledgers = [(FIXTURES / name).read_bytes() for name in LEDGERS]
	expected = expected_rows(ledgers)
	observed = rust_rows()
	if len(expected) != len(observed):
		raise ValueError(f"native packed wire row count differs: {len(expected)} versus {len(observed)}")
	for row, (wire, rust) in enumerate(zip(expected, observed, strict=True)):
		if rust != wire:
			raise ValueError(f"native packed wire differs at row {row}\nwire: {wire}\nRust: {rust}")
	print(f"Verified packed fixture ledgers SHA-256 {sha256(b''.join(ledgers)).hexdigest()}")
	print("\n".join(observed))
	digest = sha256("\n".join(observed).encode()).hexdigest()
	print(f"Compared {len(observed)} packed native wire rows SHA-256 {digest}")


if __name__ == "__main__":
	main()

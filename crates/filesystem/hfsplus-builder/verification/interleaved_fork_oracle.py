"""Independent byte-level oracle for the synthetic interleaved HFS+ image."""

import struct


PAIRED_FILE = "Paired.bin"
DIRECTORY = "More"
ITEM_COUNT = 64
BLOCK_BYTES = 4096
RUN_BLOCKS = 2
RUN_COUNT = 20
FORK_BLOCKS = RUN_BLOCKS * RUN_COUNT
DATA_BYTES = (FORK_BLOCKS - 1) * BLOCK_BYTES + 17
RESOURCE_BYTES = (FORK_BLOCKS - 1) * BLOCK_BYTES + 31
EXPECTED_IDS = {"catalog": 81, "root": 2}
HEADER_OFFSET = 1024
BLOCK_BYTES_OFFSET = 40
EXTENTS_FORK_OFFSET = 192
CATALOG_FORK_OFFSET = 272
FORK_DATA = struct.Struct(">QII")
EXTENT = struct.Struct(">II")
GROUP_WIDTH = 8
NODE_DESCRIPTOR = struct.Struct(">IIbBHH")
TREE_SUMMARY = struct.Struct(">HIIIIH")
RECORD_OFFSET = struct.Struct(">H")
OVERFLOW_KEY = struct.Struct(">HBBII")
CATALOG_KEY = struct.Struct(">HIH")
FILE_CATALOG_ID_OFFSET = 8
FILE_FORKS = (88, 168)
DATA_KIND, RESOURCE_KIND = 0, 0xff


def payloads():
	return tuple(
		bytes(seed + (offset // BLOCK_BYTES) for offset in range(length))
		for length, seed in ((DATA_BYTES, 17), (RESOURCE_BYTES, 81))
	)


def fork(image, offset):
	length, _, total = FORK_DATA.unpack_from(image, offset)
	first = offset + FORK_DATA.size
	return length, total, descriptors(image[first:first + GROUP_WIDTH * EXTENT.size])


def descriptors(raw):
	rows = list(EXTENT.iter_unpack(raw))
	used = [(start, count) for start, count in rows if count != 0]
	if rows != used + [(0, 0)] * (GROUP_WIDTH - len(used)):
		raise ValueError("nonzero or nontrailing unused extent descriptor")
	return used


def contiguous_metadata(image, offset):
	length, total, extents = fork(image, HEADER_OFFSET + offset)
	if len(extents) != 1 or extents[0][1] != total:
		raise ValueError("expected a contiguous metadata fork")
	start = extents[0][0] * BLOCK_BYTES
	return image[start:start + length]


def leaf_records(tree):
	depth, root, count, leaf, last, width = TREE_SUMMARY.unpack_from(tree, NODE_DESCRIPTOR.size)
	if width != BLOCK_BYTES or root == 0:
		raise ValueError("unexpected tree summary")
	seen = set()
	records = []
	previous = 0
	while leaf:
		if leaf in seen or leaf >= len(tree) // width:
			raise ValueError("invalid leaf chain in generated image")
		seen.add(leaf)
		node = tree[leaf * width:(leaf + 1) * width]
		following, backward, kind, height, records_in_node, _ = NODE_DESCRIPTOR.unpack_from(node)
		if backward != previous or kind != -1 or height != 1:
			raise ValueError("unexpected leaf type or backward link")
		offsets = [RECORD_OFFSET.unpack_from(node, width - (i + 1) * RECORD_OFFSET.size)[0]
			for i in range(records_in_node + 1)]
		for start, end in zip(offsets, offsets[1:]):
			if not NODE_DESCRIPTOR.size <= start < end <= width - len(offsets) * RECORD_OFFSET.size:
				raise ValueError("invalid leaf record range in generated image")
			records.append(node[start:end])
		previous, leaf = leaf, following
	if previous != last or len(records) != count:
		raise ValueError("leaf chain does not match the generated tree summary")
	return depth, records


def catalog_forks(image):
	depth, records = leaf_records(contiguous_metadata(image, CATALOG_FORK_OFFSET))
	if depth <= 1 or len(records) != 2 * (ITEM_COUNT + 3):
		raise ValueError("expected a multilevel catalog with the manifest's record count")
	seen = set()
	paired = None
	for record in records:
		width, parent, units = CATALOG_KEY.unpack_from(record)
		name = record[CATALOG_KEY.size:CATALOG_KEY.size + units * 2].decode("utf-16-be")
		key = parent, name
		if key in seen:
			raise ValueError(f"duplicate catalog record {key!r}")
		seen.add(key)
		if key == (EXPECTED_IDS["root"], PAIRED_FILE):
			file = record[RECORD_OFFSET.size + width:]
			if int.from_bytes(file[FILE_CATALOG_ID_OFFSET:FILE_CATALOG_ID_OFFSET + 4]) != EXPECTED_IDS["catalog"]:
				raise ValueError("paired file has an unexpected catalog ID")
			paired = tuple(fork(file, offset) for offset in FILE_FORKS)
	if paired is None:
		raise ValueError("paired file missing from independent catalog scan")
	print(f"Independent raw reader found {len(records)} unique catalog records at depth {depth}")
	return paired


def check_interleaved_forks(path):
	image = path.read_bytes()
	if int.from_bytes(image[HEADER_OFFSET + BLOCK_BYTES_OFFSET:][:4]) != BLOCK_BYTES:
		raise ValueError("unexpected allocation block size")
	embedded = catalog_forks(image)
	_, records = leaf_records(contiguous_metadata(image, EXTENTS_FORK_OFFSET))
	continuations = []
	for record in records:
		width, kind, pad, owner, start = OVERFLOW_KEY.unpack_from(record)
		if width != OVERFLOW_KEY.size - RECORD_OFFSET.size or pad != 0 or owner != EXPECTED_IDS["catalog"]:
			raise ValueError("unexpected overflow record key framing or owner")
		continuations.append((kind, start, descriptors(record[OVERFLOW_KEY.size:])))
	keys = [(kind, start) for kind, start, _ in continuations]
	expected_keys = [(kind, start) for kind in (DATA_KIND, RESOURCE_KIND)
		for start in (GROUP_WIDTH * RUN_BLOCKS, 2 * GROUP_WIDTH * RUN_BLOCKS)]
	if keys != expected_keys:
		raise ValueError(f"expected exactly four ordered continuation records, got {keys!r}")
	data_start = embedded[0][2][0][0]
	for (kind, (length, total, inline), expected) in zip((DATA_KIND, RESOURCE_KIND), embedded, payloads()):
		if len(inline) != GROUP_WIDTH or total != FORK_BLOCKS or length != len(expected):
			raise ValueError("unexpected embedded fork descriptor")
		extents = list(inline)
		for owner, start, group in continuations:
			if owner == kind:
				if sum(count for _, count in extents) != start:
					raise ValueError("overflow record disagrees with logical fork offset")
				extents.extend(group)
		shift = RUN_BLOCKS if kind == RESOURCE_KIND else 0
		expected_extents = [(data_start + shift + 2 * RUN_BLOCKS * index, RUN_BLOCKS)
			for index in range(RUN_COUNT)]
		if extents != expected_extents:
			raise ValueError("fork placements do not alternate as specified")
		physical = b"".join(image[start * BLOCK_BYTES:(start + count) * BLOCK_BYTES] for start, count in extents)
		if physical[:length] != expected or any(physical[length:]):
			raise ValueError("independent raw readback differs in fork contents or block padding")
	print(f"Independent raw reader matched all {len(records)} continuation keys {keys} and both complete forks")

"""Reconstruct a fresh synthetic HFS+ allocation map from its wire extents."""

from allocation_count_oracle import ALLOCATION_GEOMETRY, ALLOCATION_GEOMETRY_OFFSET
from interleaved_fork_oracle import (
	BLOCK_BYTES, CATALOG_FORK_OFFSET, EXTENTS_FORK_OFFSET, HEADER_OFFSET,
	contiguous_metadata, fork,
)


HEADER_BYTES = 512
ALLOCATION_FORK_OFFSET = 112
ATTRIBUTES_FORK_OFFSET = 352
STARTUP_FORK_OFFSET = 432
BITS_PER_BYTE = 8
SPECIAL_FORKS = {
	"allocation": ALLOCATION_FORK_OFFSET,
	"extents": EXTENTS_FORK_OFFSET,
	"catalog": CATALOG_FORK_OFFSET,
	"attributes": ATTRIBUTES_FORK_OFFSET,
	"startup": STARTUP_FORK_OFFSET,
}


def claim(used, extents, owner, total):
	for start, count in extents:
		if not 0 <= start < start + count <= total:
			raise ValueError(f"{owner} extent is outside the volume")
		blocks = set(range(start, start + count))
		if used & blocks:
			raise ValueError(f"{owner} extent overlaps allocated blocks")
		used.update(blocks)


def check_interleaved_allocation(image, file_extents):
	primary = image[HEADER_OFFSET:HEADER_OFFSET + HEADER_BYTES]
	alternate_start = len(image) - HEADER_OFFSET
	if primary != image[alternate_start:alternate_start + HEADER_BYTES]:
		raise ValueError("primary and alternate volume headers differ")
	bytes_per_block, total, free = ALLOCATION_GEOMETRY.unpack_from(primary, ALLOCATION_GEOMETRY_OFFSET)
	if bytes_per_block != BLOCK_BYTES or total * bytes_per_block != len(image) or free > total:
		raise ValueError("unexpected volume allocation geometry")

	# With this fixture's 4 KiB allocation blocks, the 1.5 KiB leading
	# and 1 KiB trailing reserved regions occupy one block each.
	reserved = {0, total - 1}
	used = set(reserved)
	for name, offset in SPECIAL_FORKS.items():
		length, count, extents = fork(primary, offset)
		if count != sum(extent_count for _, extent_count in extents) or length > count * BLOCK_BYTES:
			raise ValueError(f"{name} fork disagrees with its embedded allocation")
		claim(used, extents, name, total)
	metadata = len(used) - len(reserved)
	claim(used, file_extents, "paired file", total)
	files = len(used) - metadata - len(reserved)

	bitmap = contiguous_metadata(image, ALLOCATION_FORK_OFFSET)
	if len(bitmap) * BITS_PER_BYTE < total:
		raise ValueError("allocation bitmap does not cover the volume")
	observed = {
		index * BITS_PER_BYTE + bit
		for index, byte in enumerate(bitmap)
		for bit in range(BITS_PER_BYTE)
		if byte & (1 << (BITS_PER_BYTE - 1 - bit))
	}
	if observed != used:
		raise ValueError("allocation bitmap differs from reconstructed block ownership")
	if free != total - len(used):
		raise ValueError("volume header free count differs from reconstructed block ownership")
	print(f"Independent allocation matched both headers and bitmap across {total} blocks: "
		f"{len(reserved)} reserved, {metadata} metadata, {files} file, {free} free")

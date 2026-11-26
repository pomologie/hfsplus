"""Compare the serialized HFS+ header with the mounted Apple allocation counts."""

import struct


VOLUME_HEADER_OFFSET = 1024
ALLOCATION_GEOMETRY_OFFSET = 40
ALLOCATION_GEOMETRY = struct.Struct(">III")


def verify_mounted_allocation_counts(image, mounted):
	with image.open("rb") as stream:
		stream.seek(VOLUME_HEADER_OFFSET + ALLOCATION_GEOMETRY_OFFSET)
		block_bytes, total_blocks, free_blocks = ALLOCATION_GEOMETRY.unpack(
			stream.read(ALLOCATION_GEOMETRY.size),
		)
	if block_bytes * total_blocks != image.stat().st_size or free_blocks > total_blocks:
		raise ValueError("the volume header has unexpected allocation geometry")
	if (mounted.f_blocks * mounted.f_frsize, mounted.f_bfree * mounted.f_frsize) != (
		total_blocks * block_bytes, free_blocks * block_bytes,
	):
		raise ValueError("native total or free bytes differ from the volume header")
	print(f"macOS matched {total_blocks} total and {free_blocks} free {block_bytes}-byte allocation blocks")

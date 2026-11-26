//! Qualification of the builder's logical geometry and bounded payload I/O.

use std::{
	cell::Cell,
	io::{self, Write},
};

use hfsplus_builder::{
	RangeKind,
	volume::{
		CapacityPolicy, EntryKind, EntryMetadata, ForkSource, ManifestEntry, VolumeOptions, plan,
	},
};
use hfsplus_core::{ByteLen, ByteOffset, ByteRange, RangeError, ReadAt};
use hfsplus_format::volume::{
	ALTERNATE_HEADER_FROM_END, HEADER_BYTES, Header, PRIMARY_HEADER_OFFSET,
};

const BLOCK_BYTES: u64 = 4096;
const PAYLOAD_BYTES: u64 = 10 << 30;
const PAYLOAD_BLOCKS: u64 = PAYLOAD_BYTES / BLOCK_BYTES;
const FREE_BLOCKS: u64 = 2;
const RESERVED_BLOCKS: u64 = 2;
const TREE_BLOCKS: u64 = 3;
const BITMAP_BLOCKS: u64 = 81;
const TOTAL_BLOCKS: u64 =
	PAYLOAD_BLOCKS + RESERVED_BLOCKS + TREE_BLOCKS + BITMAP_BLOCKS + FREE_BLOCKS;
const IMAGE_BYTES: u64 = TOTAL_BLOCKS * BLOCK_BYTES;
const IO_LIMIT: usize = 32 * 1024;
const RANGE_LIMIT: usize = 16;
const DATA_BYTES: [(u64, u8); 3] = [
	(0, b'A'),
	(PAYLOAD_BYTES / 2, b'B'),
	(PAYLOAD_BYTES - 1, b'C'),
];

#[derive(Debug, Default)]
struct SparseSource {
	read_bytes: Cell<u64>,
	largest_read: Cell<usize>,
}

impl ReadAt for SparseSource {
	type Error = RangeError;

	fn len(&self) -> ByteLen {
		ByteLen(PAYLOAD_BYTES)
	}

	fn read_exact_at(&self, offset: ByteOffset, destination: &mut [u8]) -> Result<(), Self::Error> {
		let range = ByteRange::new(offset, destination.len().into(), self.len())?;
		let largest = self.largest_read.get().max(destination.len());
		assert!(largest <= IO_LIMIT);
		self.largest_read.set(largest);
		self.read_bytes.set(self.read_bytes.get() + range.len().0);
		destination.fill(0);
		for (position, byte) in DATA_BYTES {
			if (offset.0..range.end().0).contains(&position) {
				destination[usize::try_from(position - offset.0).unwrap()] = byte;
			}
		}
		Ok(())
	}
}

#[derive(Default)]
struct CountingSink(u64);

impl Write for CountingSink {
	fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
		assert!(bytes.len() <= IO_LIMIT);
		self.0 += u64::try_from(bytes.len()).unwrap();
		Ok(bytes.len())
	}

	fn flush(&mut self) -> io::Result<()> {
		Ok(())
	}
}

#[test]
fn streams_a_ten_gibibyte_sparse_source_with_bounded_reads() {
	let source = SparseSource::default();
	let mut options =
		VolumeOptions::for_capacity("Sparse source", u32::try_from(BLOCK_BYTES).unwrap()).unwrap();
	let max_metadata =
		(BITMAP_BLOCKS + TREE_BLOCKS) * BLOCK_BYTES + 2 * u64::try_from(HEADER_BYTES).unwrap();
	options.limits.max_metadata_bytes = ByteLen(max_metadata);
	options.limits.max_ranges = RANGE_LIMIT;
	let manifest = vec![ManifestEntry {
		path: "Sparse fixture",
		metadata: EntryMetadata::new(0o644),
		kind: EntryKind::File {
			data: ForkSource::whole(&source),
			resource: None,
		},
	}];
	let policy = CapacityPolicy::Auto {
		free_space: ByteLen(FREE_BLOCKS * BLOCK_BYTES),
	};
	let image = plan(options, manifest)
		.unwrap()
		.seal_with_capacity(policy)
		.unwrap();
	assert_eq!(image.image_len(), ByteLen(IMAGE_BYTES));
	assert_eq!(source.read_bytes.get(), 0);

	let mut header_bytes = [0; HEADER_BYTES];
	image
		.read_exact_at(ByteOffset(PRIMARY_HEADER_OFFSET), &mut header_bytes)
		.unwrap();
	let header = Header::decode(&header_bytes, IMAGE_BYTES).unwrap();
	assert_eq!(u64::from(header.geometry.block_size()), BLOCK_BYTES);
	assert_eq!(u64::from(header.geometry.total_blocks()), TOTAL_BLOCKS);
	assert_eq!(
		u64::from(header.forks.allocation.total_blocks),
		BITMAP_BLOCKS
	);
	assert_eq!(u64::from(header.free_blocks), FREE_BLOCKS);
	let mut alternate = [0; HEADER_BYTES];
	image
		.read_exact_at(
			ByteOffset(IMAGE_BYTES - ALTERNATE_HEADER_FROM_END),
			&mut alternate,
		)
		.unwrap();
	assert_eq!(header_bytes, alternate);

	let mut source_ranges = image
		.ranges()
		.filter(|(_, kind)| *kind == RangeKind::Source);
	let payload = source_ranges.next().unwrap().0;
	assert_eq!(payload.len(), ByteLen(PAYLOAD_BYTES));
	assert!(source_ranges.next().is_none());
	for (position, byte) in DATA_BYTES {
		let mut actual = [0];
		image
			.read_exact_at(ByteOffset(payload.start().0 + position), &mut actual)
			.unwrap();
		assert_eq!(actual, [byte]);
	}
	let mut known_zero = [u8::MAX; 32];
	image
		.read_exact_at(
			ByteOffset(payload.start().0 + PAYLOAD_BYTES / 4),
			&mut known_zero,
		)
		.unwrap();
	assert_eq!(known_zero, [0; 32]);
	assert_eq!(
		source.read_bytes.get(),
		u64::try_from(DATA_BYTES.len() + known_zero.len()).unwrap()
	);

	let mut sink = CountingSink::default();
	source.read_bytes.set(0);
	image.write_to(&mut sink).unwrap();
	assert_eq!(sink.0, IMAGE_BYTES);
	assert_eq!(source.read_bytes.get(), PAYLOAD_BYTES);
}

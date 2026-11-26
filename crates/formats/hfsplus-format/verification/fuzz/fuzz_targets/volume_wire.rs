#![no_main]
#![forbid(unsafe_code)]

use hfsplus_format::volume::{
	ALTERNATE_HEADER_FROM_END, AllocationBlock, Fork, Geometry, HEADER_BYTES, Header,
};
use libfuzzer_sys::fuzz_target;

const MIN_BLOCK_BYTES: u32 = 512;
const MIN_VOLUME_BYTES: u128 = 2_560;
const FORK_BLOCK_BYTES: u32 = 4096;
const FORK_VOLUME_BLOCKS: u32 = 2048;

fuzz_target!(|input: &[u8]| {
	if input.len() > HEADER_BYTES {
		return;
	}
	let device_bytes = input
		.first_chunk()
		.map_or(0, |value| u64::from_be_bytes(*value));
	if let Ok(header) = Header::decode(input, device_bytes) {
		let header_bytes = header.encode().unwrap();
		assert_eq!(Header::decode(&header_bytes, device_bytes), Ok(header));
		assert_eq!(
			header.geometry.alternate_header_offset(),
			device_bytes - ALTERNATE_HEADER_FROM_END
		);
	}
	let fork_geometry = Geometry::new(
		FORK_BLOCK_BYTES,
		FORK_VOLUME_BLOCKS,
		u64::from(FORK_BLOCK_BYTES) * u64::from(FORK_VOLUME_BLOCKS),
	)
	.unwrap();
	if let Ok(fork) = Fork::decode(input, fork_geometry) {
		check_fork(fork, fork_geometry);
	}
	check_geometry(input);
});

fn check_fork(fork: Fork, geometry: Geometry) {
	let wire = fork.encode(geometry).unwrap();
	assert_eq!(Fork::decode(&wire, geometry), Ok(fork));
	let block_bytes = u128::from(geometry.block_size());
	assert!(u128::from(fork.logical_bytes) <= block_bytes * u128::from(fork.total_blocks));
	let volume = u128::from(geometry.allocation_bytes());
	for extent in fork.extents() {
		let span = geometry.block_range(extent.start, extent.blocks).unwrap();
		let start = u128::from(extent.start.0) * block_bytes;
		let end = start + u128::from(extent.blocks) * block_bytes;
		assert_eq!(u128::from(span.start)..u128::from(span.end), start..end);
		assert!(end <= volume);
	}
}

fn check_geometry(input: &[u8]) {
	let (fields, _) = input.as_chunks::<4>();
	let [size, total, start, count, ..] = fields else {
		return;
	};
	let [size, total, start, count] = [*size, *total, *start, *count].map(u32::from_be_bytes);
	let device_bytes = input
		.last_chunk()
		.map_or(0, |value| u64::from_be_bytes(*value));
	let allocation = u128::from(size) * u128::from(total);
	let admitted = size >= MIN_BLOCK_BYTES
		&& size.is_power_of_two()
		&& (MIN_VOLUME_BYTES..=u128::from(device_bytes)).contains(&allocation);
	let geometry = Geometry::new(size, total, device_bytes);
	assert_eq!(geometry.is_ok(), admitted);
	if let Ok(geometry) = geometry {
		assert_eq!(u128::from(geometry.allocation_bytes()), allocation);
		let end = u64::from(start) + u64::from(count);
		let actual = geometry.block_range(AllocationBlock(start), count);
		assert_eq!(actual.is_ok(), end <= u64::from(total));
		if let Ok(range) = actual {
			assert_eq!(
				range,
				u64::from(start) * u64::from(size)..end * u64::from(size)
			);
		}
	}
}

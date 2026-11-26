use crate::{
	Error,
	volume::{AllocationBlock, Geometry},
};

const SECTOR_BYTES: u32 = 512;
const HEADER_REGION_SECTORS: u128 = 5;
const ALTERNATE_HEADER_SECTORS_FROM_END: u64 = 2;
const STANDARD_ALLOCATION_BLOCK_BYTES: u32 = 4096;

#[kani::proof]
fn geometry_matches_widened_volume_model() {
	let block_size: u32 = kani::any();
	let total_blocks: u32 = kani::any();
	let device_bytes: u64 = kani::any();
	let allocation = u128::from(block_size) * u128::from(total_blocks);
	let qualified = block_size >= SECTOR_BYTES && block_size.count_ones() == 1;
	let minimum_bytes = u128::from(SECTOR_BYTES) * HEADER_REGION_SECTORS;
	let admitted = qualified && (minimum_bytes..=u128::from(device_bytes)).contains(&allocation);

	assert!(u64::try_from(allocation).is_ok());
	match Geometry::new(block_size, total_blocks, device_bytes) {
		Ok(geometry) => {
			assert!(admitted);
			assert_eq!(geometry.block_size(), block_size);
			assert_eq!(geometry.total_blocks(), total_blocks);
			assert_eq!(u128::from(geometry.allocation_bytes()), allocation);
			assert_eq!(
				geometry.alternate_header_offset(),
				device_bytes - ALTERNATE_HEADER_SECTORS_FROM_END * u64::from(SECTOR_BYTES)
			);
		}
		Err(error) => {
			assert_eq!(error, Error::InvalidGeometry);
			assert!(!admitted);
		}
	}
	kani::cover!(block_size == 0, "zero-sized allocation block");
	kani::cover!(
		block_size >= SECTOR_BYTES && !qualified,
		"non-power-of-two allocation block"
	);
	kani::cover!(
		qualified && allocation > u128::from(device_bytes),
		"allocation exceeds source"
	);
	kani::cover!(
		admitted && block_size == STANDARD_ALLOCATION_BLOCK_BYTES && total_blocks == u32::MAX,
		"admitted large on-disk block count"
	);
}

#[kani::proof]
fn allocation_block_range_matches_widened_volume_model() {
	let block_size: u32 = kani::any();
	let total_blocks: u32 = kani::any();
	let device_bytes: u64 = kani::any();
	let Ok(geometry) = Geometry::new(block_size, total_blocks, device_bytes) else {
		return;
	};
	let start: u32 = kani::any();
	let count: u32 = kani::any();
	let end = u64::from(start) + u64::from(count);
	match geometry.block_range(AllocationBlock(start), count) {
		Ok(range) => {
			assert!(end <= u64::from(total_blocks));
			assert_eq!(range.start, u64::from(start) * u64::from(block_size));
			assert_eq!(range.end, end * u64::from(block_size));
			assert!(range.end <= device_bytes);
		}
		Err(error) => {
			assert_eq!(error, Error::InvalidGeometry);
			assert!(end > u64::from(total_blocks));
		}
	}
	kani::cover!(
		end > u64::from(u32::MAX),
		"block start plus count overflows u32"
	);
	kani::cover!(
		end == u64::from(total_blocks),
		"range terminates exactly at the allocation end"
	);
}

use hfsplus_format::volume::{AllocationBlock, Extent, Geometry};

use crate::allocation::{Allocation, AllocationError};

const SECTOR_ALLOCATION_BYTES: u32 = 512;
const STANDARD_ALLOCATION_BYTES: u32 = 4096;
const MAX_BLOCKS: u32 = 32;
const BITMAP_BITS: u128 = 8;
const MOST_SIGNIFICANT_BIT: u8 = 1 << (u8::BITS - 1);
// The first 1024 boot bytes and the following 512-byte volume header.
const FRONT_BYTES: u128 = 1536;
// The alternate 512-byte volume header and the final 512 bytes.
const BACK_BYTES: u128 = 1024;

#[kani::proof]
#[kani::unwind(10)]
fn sector_allocation_matches_bitmap_model() {
	prove_bitmap::<SECTOR_ALLOCATION_BYTES>();
}

#[kani::proof]
#[kani::unwind(10)]
fn standard_allocation_matches_bitmap_model() {
	prove_bitmap::<STANDARD_ALLOCATION_BYTES>();
}

#[kani::proof]
fn sector_allocation_matches_transition_model() {
	prove_transition::<SECTOR_ALLOCATION_BYTES>();
}

#[kani::proof]
fn standard_allocation_matches_transition_model() {
	prove_transition::<STANDARD_ALLOCATION_BYTES>();
}

fn ceil(n: u128, d: u128) -> u128 {
	(n + d - 1) / d
}

struct State {
	allocation: Allocation,
	total: u128,
	end: u128,
	next: u128,
}

fn arbitrary_state<const BLOCK_BYTES: u32>() -> Option<State> {
	let blocks: u32 = kani::any();
	kani::assume(blocks > 0 && blocks <= MAX_BLOCKS);
	let tail: u32 = kani::any();
	kani::assume(tail < BLOCK_BYTES);
	let raw_bytes = u64::from(blocks) * u64::from(BLOCK_BYTES) + u64::from(tail);
	let Ok(geometry) = Geometry::new(BLOCK_BYTES, blocks, raw_bytes) else {
		return None;
	};
	let block_bytes = u128::from(BLOCK_BYTES);
	let total = u128::from(blocks);
	let front = ceil(FRONT_BYTES, block_bytes);
	let end = total - ceil(BACK_BYTES, block_bytes);
	let bitmap_bytes = ceil(total, BITMAP_BITS);
	let bitmap_blocks = ceil(bitmap_bytes, block_bytes);
	let mut allocation = match Allocation::new(geometry) {
		Ok(allocation) => {
			assert!(front + bitmap_blocks <= end);
			allocation
		}
		Err(AllocationError::InsufficientSpace {
			requested,
			available,
		}) => {
			assert!(front + bitmap_blocks > end);
			assert_eq!(u128::from(requested), bitmap_blocks);
			assert_eq!(u128::from(available), end.saturating_sub(front));
			return None;
		}
		Err(_) => panic!("an admitted geometry has only one allocator construction failure"),
	};
	assert_eq!(
		allocation.bitmap_extent(),
		Extent {
			start: AllocationBlock(front as u32),
			blocks: bitmap_blocks as u32
		}
	);
	assert_eq!(u128::from(allocation.bitmap_bytes()), bitmap_bytes);
	assert_eq!(
		u128::from(allocation.bitmap_padding_bytes()),
		bitmap_blocks * block_bytes - bitmap_bytes
	);
	let initial = front + bitmap_blocks;
	assert_eq!(u128::from(allocation.next_allocation().0), initial);
	assert_eq!(u128::from(allocation.free_blocks()), end - initial);
	// The allocator retains only the cursor. One exact reservation realizes
	// every cursor reachable by any sequence of successful reservations.
	let reserved = u128::from(kani::any::<u32>());
	kani::assume(reserved <= end - initial);
	if reserved != 0 {
		let extent = allocation
			.allocate((reserved * block_bytes) as u64)
			.unwrap()
			.unwrap();
		assert_eq!(u128::from(extent.start.0), initial);
		assert_eq!(u128::from(extent.blocks), reserved);
	}
	let next = initial + reserved;
	assert_eq!(u128::from(allocation.next_allocation().0), next);
	assert_eq!(u128::from(allocation.free_blocks()), end - next);
	Some(State {
		allocation,
		total,
		end,
		next,
	})
}

fn prove_transition<const BLOCK_BYTES: u32>() {
	let Some(State {
		mut allocation,
		end,
		mut next,
		..
	}) = arbitrary_state::<BLOCK_BYTES>()
	else {
		return;
	};
	let length: u64 = kani::any();
	let needed = ceil(u128::from(length), u128::from(BLOCK_BYTES));
	let available = end - next;
	match allocation.allocate(length) {
		Ok(Some(extent)) => {
			assert!(0 < needed && needed <= available);
			assert_eq!(u128::from(extent.start.0), next);
			assert_eq!(u128::from(extent.blocks), needed);
			next += needed;
		}
		Ok(None) => assert_eq!(needed, 0),
		Err(AllocationError::InsufficientSpace {
			requested,
			available: actual,
		}) => {
			assert!(needed > available);
			assert_eq!(u128::from(requested), needed);
			assert_eq!(u128::from(actual), available);
		}
		Err(_) => panic!("an allocation attempt can only exhaust capacity"),
	}
	assert_eq!(u128::from(allocation.next_allocation().0), next);
	assert_eq!(u128::from(allocation.free_blocks()), end - next);
	kani::cover!(
		needed != 0 && needed == available,
		"exact final reservation"
	);
	kani::cover!(needed > available, "reservation exceeds available blocks");
}

fn prove_bitmap<const BLOCK_BYTES: u32>() {
	let Some(State {
		allocation,
		total,
		end,
		next,
	}) = arbitrary_state::<BLOCK_BYTES>()
	else {
		return;
	};
	let bitmap_bytes = ceil(total, BITMAP_BITS);
	kani::cover!(next == end, "requests exhaust exactly all free blocks");
	kani::cover!(
		next < end && total % BITMAP_BITS != 0,
		"free space and partial final bitmap byte"
	);
	let bytes = allocation.encode_bitmap(bitmap_bytes as usize).unwrap();
	assert_eq!(bytes.len() as u128, bitmap_bytes);
	// An arbitrary in-bounds bit quantifies over the entire encoded bitmap.
	let bit = u128::from(kani::any::<u32>());
	kani::assume(bit < bitmap_bytes * BITMAP_BITS);
	let index = (bit / BITMAP_BITS) as usize;
	let bit_from_high = (bit % BITMAP_BITS) as u32;
	let marked = bytes[index] & (MOST_SIGNIFICANT_BIT >> bit_from_high) != 0;
	assert_eq!(marked, bit < next || (end <= bit && bit < total));
}

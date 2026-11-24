//! Deterministic contiguous block allocation and the HFS+ allocation file.

use std::{collections::TryReserveError, error::Error, fmt};

use hfsplus_format::volume::{
	AllocationBlock, Extent, Geometry, HEADER_BYTES, PRIMARY_HEADER_OFFSET,
};

const BLOCKS_PER_BITMAP_BYTE: u32 = u8::BITS;
const TRAILING_RESERVED_BYTES: u64 = 512;
const PRIMARY_RESERVED_BYTES: u64 = PRIMARY_HEADER_OFFSET + HEADER_BYTES as u64;
const ALTERNATE_RESERVED_BYTES: u64 = HEADER_BYTES as u64 + TRAILING_RESERVED_BYTES;

/// A requested allocation or its bitmap could not be represented within the volume or budget.
#[derive(Debug)]
#[non_exhaustive]
pub enum AllocationError {
	/// The described blocks do not cover all full blocks in the raw volume.
	IncompleteGeometry,
	/// The requested extent exceeds the remaining contiguous free space.
	InsufficientSpace {
		/// Requested allocation blocks.
		requested: u64,
		/// Remaining contiguous allocation blocks.
		available: u32,
	},
	/// The allocation file payload exceeds the bytes admitted by the caller.
	BitmapLimit {
		/// Required bitmap payload bytes, before block padding.
		required: u64,
		/// Maximum bitmap payload bytes.
		limit: usize,
	},
	/// Memory for the admitted allocation bitmap could not be reserved.
	BitmapMemory(TryReserveError),
}

impl fmt::Display for AllocationError {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::IncompleteGeometry => {
				formatter.write_str("HFS+ geometry omits full allocation blocks")
			}
			Self::InsufficientSpace {
				requested,
				available,
			} => {
				write!(
					formatter,
					"requested {requested} allocation blocks with {available} available"
				)
			}
			Self::BitmapLimit { required, limit } => {
				write!(
					formatter,
					"allocation bitmap requires {required} bytes with a limit of {limit}"
				)
			}
			Self::BitmapMemory(error) => {
				write!(formatter, "cannot reserve allocation bitmap: {error}")
			}
		}
	}
}

impl Error for AllocationError {
	fn source(&self) -> Option<&(dyn Error + 'static)> {
		match self {
			Self::BitmapMemory(error) => Some(error),
			_ => None,
		}
	}
}

/// Assigns contiguous extents in call order for a new HFS+ volume.
///
/// Boot bytes and both header regions are reserved, including the final whole
/// allocation block even when the alternate header begins beyond it. The bitmap
/// itself is assigned the first usable extent. Failed extent allocations leave
/// the cursor unchanged.
#[derive(Debug)]
pub struct Allocation {
	geometry: Geometry,
	bitmap_extent: Extent,
	next: u32,
	suffix: u32,
}

impl Allocation {
	/// Reserves boot and header regions and allocates the allocation file.
	///
	/// # Errors
	/// Rejects geometry that omits full allocation blocks and volumes without
	/// room for their reserved regions and allocation bitmap.
	pub fn new(geometry: Geometry) -> Result<Self, AllocationError> {
		let block_size = u64::from(geometry.block_size());
		// Geometry guarantees the device covers the described allocation blocks.
		let device_bytes = geometry.alternate_header_offset() + ALTERNATE_RESERVED_BYTES;
		if device_bytes - geometry.allocation_bytes() >= block_size {
			return Err(AllocationError::IncompleteGeometry);
		}

		// The format guarantees at least one 512-byte sector per allocation block.
		let prefix = PRIMARY_RESERVED_BYTES.div_ceil(block_size) as u32;
		let reserved_tail = ALTERNATE_RESERVED_BYTES.div_ceil(block_size) as u32;
		let suffix = geometry.total_blocks() - reserved_tail;
		let bitmap_blocks = bitmap_len(geometry).div_ceil(geometry.block_size());
		let available = suffix.saturating_sub(prefix);
		if bitmap_blocks > available {
			return Err(AllocationError::InsufficientSpace {
				requested: u64::from(bitmap_blocks),
				available,
			});
		}

		Ok(Self {
			geometry,
			bitmap_extent: Extent {
				start: AllocationBlock(prefix),
				blocks: bitmap_blocks,
			},
			next: prefix + bitmap_blocks,
			suffix,
		})
	}

	/// Assigns the next contiguous extent for `logical_bytes`, rounded up to
	/// whole blocks. An empty fork does not require an extent.
	///
	/// # Errors
	/// Returns an error if the requested extent exceeds the free space.
	pub fn allocate(&mut self, logical_bytes: u64) -> Result<Option<Extent>, AllocationError> {
		if logical_bytes == 0 {
			return Ok(None);
		}
		let requested = logical_bytes.div_ceil(u64::from(self.geometry.block_size()));
		let available = self.free_blocks();
		if requested > u64::from(available) {
			return Err(AllocationError::InsufficientSpace {
				requested,
				available,
			});
		}

		// The available count is a u32 and bounds the conversion and cursor advance.
		let blocks = requested as u32;
		let extent = Extent {
			start: AllocationBlock(self.next),
			blocks,
		};
		self.next += blocks;
		Ok(Some(extent))
	}

	/// Returns the extent assigned to the allocation file.
	#[must_use]
	pub fn bitmap_extent(&self) -> Extent {
		self.bitmap_extent
	}

	/// Returns the byte count required for one bit per volume allocation block.
	#[must_use]
	pub fn bitmap_bytes(&self) -> u64 {
		u64::from(bitmap_len(self.geometry))
	}

	/// Returns the whole-block byte length of the allocation file fork.
	#[must_use]
	pub fn bitmap_file_bytes(&self) -> u64 {
		u64::from(self.bitmap_extent.blocks) * u64::from(self.geometry.block_size())
	}

	/// Returns the explicit zero bytes to append after the encoded bitmap payload.
	#[must_use]
	pub fn bitmap_padding_bytes(&self) -> u64 {
		self.bitmap_file_bytes() - self.bitmap_bytes()
	}

	/// Returns the exact number of unassigned allocation blocks.
	#[must_use]
	pub fn free_blocks(&self) -> u32 {
		self.suffix - self.next
	}

	/// Returns the first free block, or the first reserved tail block when full.
	#[must_use]
	pub fn next_allocation(&self) -> AllocationBlock {
		AllocationBlock(self.next)
	}

	/// Encodes the bytes covering real allocation blocks, leaving unused bits
	/// clear in the final byte. Emit the fork's remaining block padding as zeros.
	///
	/// # Errors
	/// Returns an error when the payload exceeds `limit` or its buffer cannot
	/// be allocated.
	pub fn encode_bitmap(&self, limit: usize) -> Result<Box<[u8]>, AllocationError> {
		let required = self.bitmap_bytes();
		let size = usize::try_from(required)
			.ok()
			.filter(|size| *size <= limit)
			.ok_or(AllocationError::BitmapLimit { required, limit })?;
		let mut bitmap = Vec::new();
		bitmap
			.try_reserve_exact(size)
			.map_err(AllocationError::BitmapMemory)?;
		bitmap.resize(size, 0);
		mark_used(&mut bitmap, 0, self.next);
		mark_used(&mut bitmap, self.suffix, self.geometry.total_blocks());
		Ok(bitmap.into_boxed_slice())
	}
}

fn bitmap_len(geometry: Geometry) -> u32 {
	geometry.total_blocks().div_ceil(BLOCKS_PER_BITMAP_BYTE)
}

fn mark_used(bitmap: &mut [u8], start: u32, end: u32) {
	// The caller supplies a nonempty range within the encoded block count.
	// Their byte indexes therefore fit the already admitted bitmap buffer.
	let first_byte = (start / BLOCKS_PER_BITMAP_BYTE) as usize;
	let last_block = end - 1;
	let last_byte = (last_block / BLOCKS_PER_BITMAP_BYTE) as usize;
	let first_mask = u8::MAX >> (start % BLOCKS_PER_BITMAP_BYTE);
	let last_mask = u8::MAX << (BLOCKS_PER_BITMAP_BYTE - 1 - last_block % BLOCKS_PER_BITMAP_BYTE);
	if first_byte == last_byte {
		bitmap[first_byte] |= first_mask & last_mask;
	} else {
		bitmap[first_byte] |= first_mask;
		bitmap[first_byte + 1..last_byte].fill(u8::MAX);
		bitmap[last_byte] |= last_mask;
	}
}

#[cfg(test)]
mod tests {
	use super::{Allocation, AllocationError, mark_used};
	use hfsplus_format::volume::{AllocationBlock, Extent, Geometry};

	#[test]
	fn reserves_first_three_and_last_two_small_blocks() {
		let geometry = Geometry::new(512, 7, 7 * 512).unwrap();
		let mut allocation = Allocation::new(geometry).unwrap();
		assert_eq!(
			allocation.bitmap_extent(),
			Extent {
				start: AllocationBlock(3),
				blocks: 1
			}
		);
		assert_eq!(allocation.bitmap_bytes(), 1);
		assert_eq!(allocation.bitmap_file_bytes(), 512);
		assert_eq!(allocation.bitmap_padding_bytes(), 511);
		assert_eq!(&*allocation.encode_bitmap(1).unwrap(), &[0xf6]);
		assert_eq!(allocation.free_blocks(), 1);
		assert_eq!(allocation.allocate(0).unwrap(), None);
		assert!(matches!(
			allocation.allocate(513),
			Err(AllocationError::InsufficientSpace {
				requested: 2,
				available: 1
			})
		));
		assert_eq!(allocation.next_allocation(), AllocationBlock(4));
		assert_eq!(
			allocation.allocate(512).unwrap(),
			Some(Extent {
				start: AllocationBlock(4),
				blocks: 1
			})
		);
		assert_eq!(allocation.next_allocation(), AllocationBlock(5));
		assert_eq!(allocation.free_blocks(), 0);
		assert_eq!(&*allocation.encode_bitmap(1).unwrap(), &[0xfe]);
	}

	#[test]
	fn reserves_the_last_complete_block_when_header_starts_in_the_device_tail() {
		let geometry = Geometry::new(4096, 17, 18 * 4096 - 1).unwrap();
		assert!(geometry.alternate_header_offset() > geometry.allocation_bytes());
		let mut allocation = Allocation::new(geometry).unwrap();
		assert_eq!(
			allocation.allocate(4097).unwrap(),
			Some(Extent {
				start: AllocationBlock(2),
				blocks: 2
			})
		);
		assert_eq!(allocation.free_blocks(), 12);
		assert_eq!(&*allocation.encode_bitmap(3).unwrap(), &[0xf0, 0, 0x80]);
	}

	#[test]
	fn excludes_unsuitable_geometry_and_shapes_the_bitmap_budget() {
		let underspecified = Geometry::new(4096, 1, 8192).unwrap();
		assert!(matches!(
			Allocation::new(underspecified),
			Err(AllocationError::IncompleteGeometry)
		));
		let one_block = Geometry::new(4096, 1, 4096).unwrap();
		assert!(matches!(
			Allocation::new(one_block),
			Err(AllocationError::InsufficientSpace { .. })
		));
		let geometry = Geometry::new(1024, 257, 257 * 1024).unwrap();
		let mut allocation = Allocation::new(geometry).unwrap();
		assert_eq!(
			allocation.bitmap_extent(),
			Extent {
				start: AllocationBlock(2),
				blocks: 1
			}
		);
		assert!(matches!(
			allocation.encode_bitmap(32),
			Err(AllocationError::BitmapLimit {
				required: 33,
				limit: 32
			})
		));
		assert!(matches!(
			allocation.allocate(u64::MAX),
			Err(AllocationError::InsufficientSpace { .. })
		));
		assert_eq!(allocation.next_allocation(), AllocationBlock(3));
		let bitmap = allocation.encode_bitmap(33).unwrap();
		assert_eq!(bitmap[0], 0xe0);
		assert_eq!(bitmap[32], 0x80);
	}

	#[test]
	fn reproduces_bitmap_bits_observed_in_the_native_2048_block_fixture() {
		// Native allocation file in the 8 MiB fixture, projected to the extents
		// declared by its volume header. The native layout leaves an interior gap.
		let mut bitmap = [0; 256];
		mark_used(&mut bitmap, 0, 34);
		mark_used(&mut bitmap, 194, 210);
		mark_used(&mut bitmap, 2047, 2048);
		let observed_nonzero = [
			(0, 0xff),
			(1, 0xff),
			(2, 0xff),
			(3, 0xff),
			(4, 0xc0),
			(24, 0x3f),
			(25, 0xff),
			(26, 0xc0),
			(255, 0x01),
		];
		let nonzero: Vec<_> = bitmap
			.into_iter()
			.enumerate()
			.filter(|(_, byte)| *byte != 0)
			.collect();
		assert_eq!(nonzero, observed_nonzero);
	}

	#[test]
	fn contiguous_layout_matches_an_independent_bit_model() {
		for block_size in [512, 1024, 4096, 16384] {
			for total in 8..=65 {
				let device_bytes = u64::from(block_size) * u64::from(total);
				let geometry = Geometry::new(block_size, total, device_bytes).unwrap();
				let expected_head = match block_size {
					512 => 3,
					1024 => 2,
					_ => 1,
				};
				let expected_tail = if block_size == 512 { 2 } else { 1 };
				let mut used = vec![false; total as usize];
				used[..expected_head + 1].fill(true);
				used[total as usize - expected_tail..].fill(true);
				let mut allocation = Allocation::new(geometry).unwrap();
				for (request, blocks) in [
					(1, 1),
					(u64::from(block_size), 1),
					(u64::from(block_size) + 1, 2),
				] {
					let expected_start = used
						.windows(blocks)
						.position(|run| run.iter().all(|used| !used));
					match (allocation.allocate(request), expected_start) {
						(Ok(Some(extent)), Some(start)) => {
							assert_eq!(extent.start, AllocationBlock(start as u32));
							assert_eq!(extent.blocks, blocks as u32);
							used[start..start + blocks].fill(true);
						}
						(Err(AllocationError::InsufficientSpace { .. }), None) => {}
						(actual, expected) => {
							panic!("allocation {actual:?} differs from expected start {expected:?}")
						}
					}
				}
				let bitmap = allocation.encode_bitmap(total as usize).unwrap();
				assert_eq!(
					allocation.free_blocks() as usize,
					used.iter().filter(|block| !**block).count()
				);
				for (index, expected) in used.into_iter().enumerate() {
					assert_eq!(bitmap[index / 8] & (0x80 >> (index % 8)) != 0, expected);
				}
			}
		}
	}

	#[test]
	fn reserves_every_block_of_a_bitmap_that_crosses_a_block_boundary() {
		let geometry = Geometry::new(512, 4097, 4097 * 512).unwrap();
		let allocation = Allocation::new(geometry).unwrap();
		assert_eq!(
			allocation.bitmap_extent(),
			Extent {
				start: AllocationBlock(3),
				blocks: 2
			}
		);
		assert_eq!(allocation.bitmap_bytes(), 513);
		assert_eq!(allocation.bitmap_file_bytes(), 1024);
		assert_eq!(allocation.bitmap_padding_bytes(), 511);
		let bitmap = allocation.encode_bitmap(513).unwrap();
		assert_eq!(bitmap[0], 0xf8);
		assert_eq!(bitmap[511], 0x01);
		assert_eq!(bitmap[512], 0x80);
		assert_eq!(allocation.free_blocks(), 4090);
	}
}

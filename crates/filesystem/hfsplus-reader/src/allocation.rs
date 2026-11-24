//! Read-only allocation bitmap and special-file ownership checks.

use alloc::{collections::TryReserveError, vec::Vec};
use core::{fmt, ops::Range};

use hfsplus_core::{ByteLen, ByteOffset, ReadAt};
use hfsplus_format::volume::{
	ALTERNATE_HEADER_FROM_END, AllocationBlock, HEADER_BYTES, PRIMARY_HEADER_OFFSET,
};

use crate::{
	Error, SpecialFile, Volume,
	overflow::{OverflowError, OverflowLimits},
};

const BITMAP_CHUNK_BYTES: usize = 4096;
const DEFAULT_MAX_BITMAP_BYTES: u64 = 64 << 20;
const DEFAULT_MAX_RANGES: usize = 4096;
const SPECIAL_FILES: [SpecialFile; 5] = [
	SpecialFile::Allocation,
	SpecialFile::Extents,
	SpecialFile::Catalog,
	SpecialFile::Attributes,
	SpecialFile::Startup,
];

/// Resource bounds for the allocation bitmap and its metadata owners.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllocationLimits {
	/// Maximum logical bitmap bytes to scan, excluding file padding.
	pub max_bitmap_bytes: u64,
	/// Maximum number of metadata extents including the two header regions.
	pub max_ranges: usize,
	/// Bounds for resolving each special-file fork.
	pub overflow: OverflowLimits,
}

impl Default for AllocationLimits {
	fn default() -> Self {
		Self {
			max_bitmap_bytes: DEFAULT_MAX_BITMAP_BYTES,
			max_ranges: DEFAULT_MAX_RANGES,
			overflow: OverflowLimits::default(),
		}
	}
}

/// Owner of an allocation range checked by this operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataOwner {
	PrimaryRegion,
	AlternateRegion,
	SpecialFile(SpecialFile),
}

/// Checked allocation counts. Ownership of other allocated blocks requires a
/// separate catalog and attribute traversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct AllocationReport {
	pub total_blocks: u32,
	pub free_blocks: u32,
	pub metadata_blocks: u32,
	pub other_allocated_blocks: u32,
}

/// A failure to check allocation accounting.
#[derive(Debug)]
#[non_exhaustive]
pub enum AllocationError<E> {
	BitmapLimit {
		required: u64,
		limit: u64,
	},
	RangeLimit,
	Memory(TryReserveError),
	Fork {
		file: SpecialFile,
		error: OverflowError<E>,
	},
	Read(Error<E>),
	BitmapLength {
		required: u64,
		actual: u64,
	},
	ExtentBounds {
		owner: MetadataOwner,
	},
	Overlap {
		block: AllocationBlock,
		first: MetadataOwner,
		second: MetadataOwner,
	},
	MarkedFree {
		block: AllocationBlock,
		owner: MetadataOwner,
	},
	FreeBlocks {
		declared: u32,
		counted: u32,
	},
}

impl<E: fmt::Display> fmt::Display for AllocationError<E> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::BitmapLimit { required, limit } => write!(
				f,
				"allocation bitmap requires {required} bytes with a limit of {limit}"
			),
			Self::RangeLimit => f.write_str("metadata allocation range limit exceeded"),
			Self::Memory(error) => write!(f, "cannot reserve allocation ranges: {error}"),
			Self::Fork { file, error } => write!(f, "cannot admit {file:?} fork: {error}"),
			Self::Read(error) => error.fmt(f),
			Self::BitmapLength { required, actual } => write!(
				f,
				"allocation fork has {actual} bytes with {required} required"
			),
			Self::ExtentBounds { owner } => {
				write!(f, "allocation range for {owner:?} is outside the volume")
			}
			Self::Overlap {
				block,
				first,
				second,
			} => write!(
				f,
				"allocation block {} is shared by {first:?} and {second:?}",
				block.0
			),
			Self::MarkedFree { block, owner } => write!(
				f,
				"allocation block {} for {owner:?} is marked free",
				block.0
			),
			Self::FreeBlocks { declared, counted } => write!(
				f,
				"volume declares {declared} free blocks but the allocation file contains {counted}"
			),
		}
	}
}

impl<E: core::error::Error + 'static> core::error::Error for AllocationError<E> {
	fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
		match self {
			Self::Memory(error) => Some(error),
			Self::Fork { error, .. } => Some(error),
			Self::Read(error) => Some(error),
			_ => None,
		}
	}
}

#[derive(Debug)]
struct Claim {
	blocks: Range<u32>,
	owner: MetadataOwner,
}

impl<S: ReadAt> Volume<S> {
	/// Scans the real allocation bits and reconciles their free count with the
	/// primary header. Both header regions and every resolved special-file
	/// extent must be disjoint and marked allocated. File payload ownership
	/// and B-tree integrity are separate checks.
	///
	/// # Errors
	/// Fails on an unsupported volume, invalid geometry, missing or crossed
	/// allocation claims, source errors, or exhausted limits.
	pub fn check_metadata_allocation(
		&self,
		limits: AllocationLimits,
	) -> Result<AllocationReport, AllocationError<S::Error>> {
		let header = self.header();
		let geometry = header.geometry;
		let total = geometry.total_blocks();
		let bitmap_bytes = u64::from(total.div_ceil(u8::BITS));
		if bitmap_bytes > limits.max_bitmap_bytes {
			return Err(AllocationError::BitmapLimit {
				required: bitmap_bytes,
				limit: limits.max_bitmap_bytes,
			});
		}
		let open = |file| {
			self.open_resolved_special_fork(file, limits.overflow)
				.map_err(|error| AllocationError::Fork { file, error })
		};
		let bitmap = open(SpecialFile::Allocation)?;
		if bitmap.len().0 < bitmap_bytes {
			return Err(AllocationError::BitmapLength {
				required: bitmap_bytes,
				actual: bitmap.len().0,
			});
		}
		let block_bytes = u64::from(geometry.block_size());
		let head = (PRIMARY_HEADER_OFFSET + HEADER_BYTES as u64).div_ceil(block_bytes) as u32;
		let tail = ALTERNATE_HEADER_FROM_END.div_ceil(block_bytes) as u32;
		let mut claims = Vec::new();
		push_claim(
			&mut claims,
			0,
			head,
			total,
			MetadataOwner::PrimaryRegion,
			limits.max_ranges,
		)?;
		push_claim(
			&mut claims,
			total - tail,
			tail,
			total,
			MetadataOwner::AlternateRegion,
			limits.max_ranges,
		)?;
		for file in SPECIAL_FILES {
			let owner = MetadataOwner::SpecialFile(file);
			if file == SpecialFile::Allocation {
				for extent in bitmap.extents() {
					push_claim(
						&mut claims,
						extent.start.0,
						extent.blocks,
						total,
						owner,
						limits.max_ranges,
					)?;
				}
			} else {
				for extent in open(file)?.extents() {
					push_claim(
						&mut claims,
						extent.start.0,
						extent.blocks,
						total,
						owner,
						limits.max_ranges,
					)?;
				}
			}
		}
		claims.sort_unstable_by_key(|claim| claim.blocks.start);
		for pair in claims.windows(2) {
			if pair[1].blocks.start < pair[0].blocks.end {
				return Err(AllocationError::Overlap {
					block: AllocationBlock(pair[1].blocks.start),
					first: pair[0].owner,
					second: pair[1].owner,
				});
			}
		}
		let metadata_blocks = claims
			.iter()
			.map(|claim| claim.blocks.end - claim.blocks.start)
			.sum();
		let allocated = scan_bitmap(&bitmap, bitmap_bytes, total, &claims)?;
		let free_blocks = total - allocated;
		if free_blocks != header.free_blocks {
			return Err(AllocationError::FreeBlocks {
				declared: header.free_blocks,
				counted: free_blocks,
			});
		}
		Ok(AllocationReport {
			total_blocks: total,
			free_blocks,
			metadata_blocks,
			other_allocated_blocks: allocated - metadata_blocks,
		})
	}
}

fn push_claim<E>(
	claims: &mut Vec<Claim>,
	start: u32,
	length: u32,
	total: u32,
	owner: MetadataOwner,
	limit: usize,
) -> Result<(), AllocationError<E>> {
	let end = start
		.checked_add(length)
		.filter(|&end| end <= total)
		.ok_or(AllocationError::ExtentBounds { owner })?;
	if claims.len() >= limit {
		return Err(AllocationError::RangeLimit);
	}
	claims.try_reserve(1).map_err(AllocationError::Memory)?;
	claims.push(Claim {
		blocks: start..end,
		owner,
	});
	Ok(())
}

fn scan_bitmap<E>(
	bitmap: &impl ReadAt<Error = Error<E>>,
	bitmap_bytes: u64,
	total: u32,
	mut claims: &[Claim],
) -> Result<u32, AllocationError<E>> {
	let mut buffer = [0; BITMAP_CHUNK_BYTES];
	let mut offset = 0;
	let mut allocated = 0;
	while offset < bitmap_bytes {
		let len = (bitmap_bytes - offset).min(ByteLen::from(buffer.len()).0) as usize;
		bitmap
			.read_exact_at(ByteOffset(offset), &mut buffer[..len])
			.map_err(AllocationError::Read)?;
		for &byte in &buffer[..len] {
			// The bitmap byte is within ceil(total / 8), so its first block fits in u32.
			let start = (offset * u64::from(u8::BITS)) as u32;
			let end = start.saturating_add(u8::BITS).min(total);
			allocated += (byte & mask(0, end - start)).count_ones();
			while let Some((claim, rest)) = claims.split_first() {
				if claim.blocks.start >= end {
					break;
				}
				let first = claim.blocks.start.max(start) - start;
				let limit = claim.blocks.end.min(end) - start;
				let missing = mask(first, limit) & !byte;
				if missing != 0 {
					return Err(AllocationError::MarkedFree {
						block: AllocationBlock(start + missing.leading_zeros()),
						owner: claim.owner,
					});
				}
				if claim.blocks.end > end {
					break;
				}
				claims = rest;
			}
			offset += 1;
		}
	}
	Ok(allocated)
}

fn mask(first: u32, limit: u32) -> u8 {
	(u8::MAX >> first) & (u8::MAX << (u8::BITS - limit))
}

#[cfg(test)]
mod tests {
	use core::convert::Infallible;

	use hfsplus_core::ByteRange;

	use super::*;

	struct Bitmap([u8; 2]);

	impl ReadAt for Bitmap {
		type Error = Error<Infallible>;

		fn len(&self) -> ByteLen {
			self.0.len().into()
		}

		fn read_exact_at(&self, offset: ByteOffset, buffer: &mut [u8]) -> Result<(), Self::Error> {
			ByteRange::new(offset, buffer.len().into(), self.len()).map_err(Error::Bounds)?;
			let start = offset.0 as usize;
			buffer.copy_from_slice(&self.0[start..start + buffer.len()]);
			Ok(())
		}
	}

	#[test]
	fn ownership_checks_span_bytes_and_ignore_final_padding_bits() {
		let owners = [Claim {
			blocks: 5..11,
			owner: MetadataOwner::PrimaryRegion,
		}];
		let all_marked = Bitmap([0b0000_0111, 0b1111_1111]);
		assert_eq!(scan_bitmap(&all_marked, 2, 11, &owners).unwrap(), 6);
		let missing = Bitmap([0b0000_0111, 0b1011_1111]);
		assert!(matches!(
			scan_bitmap(&missing, 2, 11, &owners),
			Err(AllocationError::MarkedFree {
				block: AllocationBlock(9),
				..
			})
		));
	}
}

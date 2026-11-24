use core::{cmp::Ordering, mem::size_of};

use crate::{
	Error,
	binary::u32_at,
	volume::{AllocationBlock, Extent, FORK_BYTES, Fork, Geometry},
};

use super::{
	INLINE_RECORD, INLINE_START_BLOCK, InlineRecord, Key, RECORD_KIND_OFFSET,
	reject_compressed_attribute,
};

const FORK_RECORD: u32 = 0x20;
const EXTENTS_RECORD: u32 = 0x30;
const DESCRIPTOR_OFFSET: usize = size_of::<u32>() * 2;
const EXTENT_START_OFFSET: usize = 0;
const EXTENT_COUNT_OFFSET: usize = size_of::<u32>();
const EXTENT_BYTES: usize = size_of::<u32>() * 2;
const EMPTY_EXTENT: Extent = Extent {
	start: AllocationBlock(0),
	blocks: 0,
};
/// Extent descriptor slots in an attribute fork or continuation record.
pub const EXTENTS_PER_RECORD: usize = 8;
const EXTENT_RECORD_BYTES: usize = DESCRIPTOR_OFFSET + EXTENT_BYTES * EXTENTS_PER_RECORD;
const FORK_RECORD_BYTES: usize = DESCRIPTOR_OFFSET + FORK_BYTES;

/// One attribute B-tree leaf record and its decoded representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record<'bytes> {
	/// Owner, exact name, and the first logical block covered by the record.
	pub key: Key<'bytes>,
	/// Inline data or the checked allocation descriptor from the record.
	pub value: Value<'bytes>,
}

/// Stored attribute representation. Continuation order and ownership are
/// checked by the tree reader against the anchor record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Value<'bytes> {
	/// Only the declared value bytes, excluding alignment padding.
	Inline(&'bytes [u8]),
	/// Logical EOF, total allocation, and the first group of extents.
	Fork(Fork),
	/// A nonempty extent group whose logical size covers this entire allocation.
	Extents(Fork),
}

impl<'bytes> Record<'bytes> {
	/// Decodes a supported stored attribute record and validates all embedded
	/// extent ranges against the owning volume geometry.
	/// # Errors
	/// Rejects compressed-file control records, unsupported kinds, truncated
	/// descriptors, invalid starts, and malformed embedded extents.
	pub fn decode(bytes: &'bytes [u8], geometry: Geometry) -> Result<Self, Error> {
		let (key, body) = Key::split(bytes)?;
		let kind = u32_at(body, RECORD_KIND_OFFSET)?;
		reject_compressed_attribute(key.name.units())?;
		let value = match kind {
			INLINE_RECORD => Value::Inline(InlineRecord::decode_value(key, body)?.value),
			FORK_RECORD => {
				if key.start_block != INLINE_START_BLOCK {
					return Err(Error::InvalidAttribute);
				}
				let bytes = descriptor(body, FORK_RECORD_BYTES)?;
				Value::Fork(Fork::decode(bytes, geometry)?)
			}
			EXTENTS_RECORD => {
				if key.start_block == INLINE_START_BLOCK {
					return Err(Error::InvalidAttribute);
				}
				Value::Extents(decode_extents(
					descriptor(body, EXTENT_RECORD_BYTES)?,
					geometry,
				)?)
			}
			_ => return Err(Error::UnsupportedAttributeRecord(kind)),
		};
		Ok(Self { key, value })
	}
}

fn descriptor(body: &[u8], exact_bytes: usize) -> Result<&[u8], Error> {
	match body.len().cmp(&exact_bytes) {
		Ordering::Less => Err(Error::Truncated),
		Ordering::Greater => Err(Error::InvalidAttribute),
		Ordering::Equal => Ok(&body[DESCRIPTOR_OFFSET..]),
	}
}

fn decode_extents(bytes: &[u8], geometry: Geometry) -> Result<Fork, Error> {
	let mut extents = [EMPTY_EXTENT; EXTENTS_PER_RECORD];
	let mut blocks = 0u32;
	for (extent, chunk) in extents.iter_mut().zip(bytes.as_chunks::<EXTENT_BYTES>().0) {
		let start = AllocationBlock(u32_at(chunk, EXTENT_START_OFFSET)?);
		let count = u32_at(chunk, EXTENT_COUNT_OFFSET)?;
		blocks = blocks.checked_add(count).ok_or(Error::InvalidFork)?;
		*extent = Extent {
			start,
			blocks: count,
		};
	}
	if blocks == 0 {
		return Err(Error::InvalidAttribute);
	}
	let bytes = u64::from(blocks) * u64::from(geometry.block_size());
	Fork::new(bytes, 0, blocks, &extents, geometry)
}

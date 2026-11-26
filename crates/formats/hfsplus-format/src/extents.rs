//! Wire records for the extents overflow B-tree.

use crate::{
	Error,
	binary::{u16_at, u32_at},
	volume::{CatalogId, Extent},
};

/// Extents in the catalog's embedded record or an overflow continuation.
pub const EXTENTS_PER_RECORD: usize = 8;
/// Encoded length of one physical extent descriptor.
pub const EXTENT_BYTES: usize = 2 * size_of::<u32>();
/// Fixed key length excluding the key-length field itself.
pub const KEY_LENGTH: u16 = 10;
/// Full encoded key length.
pub const KEY_BYTES: usize = size_of::<u16>() + KEY_LENGTH as usize;
/// Encoded value length of a leaf record.
pub const RECORD_VALUE_BYTES: usize = EXTENTS_PER_RECORD * EXTENT_BYTES;
/// Encoded length of a leaf record including its key.
pub const LEAF_RECORD_BYTES: usize = KEY_BYTES + RECORD_VALUE_BYTES;

const FORK_KIND_OFFSET: usize = size_of::<u16>();
const CATALOG_ID_OFFSET: usize = FORK_KIND_OFFSET + 2;
const START_BLOCK_OFFSET: usize = CATALOG_ID_OFFSET + size_of::<u32>();

/// Which stored fork an overflow key identifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum ForkKind {
	/// Data fork.
	Data = 0,
	/// Resource fork.
	Resource = 0xff,
}

/// Extents keys compare in catalog ID, fork kind, and logical block order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key {
	/// Catalog ID of the storage owner.
	pub id: CatalogId,
	/// Fork to which the continuation belongs.
	pub kind: ForkKind,
	/// Allocation-block index within the logical fork.
	pub start_block: u32,
}

impl Key {
	/// Decodes the common key prefix of an index or leaf record.
	///
	/// # Errors
	/// Rejects a truncated key, invalid key length, or unknown fork kind.
	pub fn decode(record: &[u8]) -> Result<Self, Error> {
		let key = record.get(..KEY_BYTES).ok_or(Error::InvalidNode)?;
		if u16_at(key, 0)? != KEY_LENGTH {
			return Err(Error::InvalidNode);
		}
		let kind = match key[FORK_KIND_OFFSET] {
			value if value == ForkKind::Data as u8 => ForkKind::Data,
			value if value == ForkKind::Resource as u8 => ForkKind::Resource,
			_ => return Err(Error::InvalidNode),
		};
		Ok(Self {
			id: CatalogId(u32_at(key, CATALOG_ID_OFFSET)?),
			kind,
			start_block: u32_at(key, START_BLOCK_OFFSET)?,
		})
	}

	/// Encodes a continuation, padding unused extent slots with zeroes.
	///
	/// # Errors
	/// Rejects an empty continuation, empty supplied extents, or too many extents.
	pub fn encode_record(self, extents: &[Extent]) -> Result<[u8; LEAF_RECORD_BYTES], Error> {
		if extents.is_empty()
			|| extents.len() > EXTENTS_PER_RECORD
			|| extents.iter().any(|extent| extent.blocks == 0)
		{
			return Err(Error::InvalidFork);
		}
		let mut record = [0; LEAF_RECORD_BYTES];
		record[..FORK_KIND_OFFSET].copy_from_slice(&KEY_LENGTH.to_be_bytes());
		record[FORK_KIND_OFFSET] = self.kind as u8;
		record[CATALOG_ID_OFFSET..START_BLOCK_OFFSET].copy_from_slice(&self.id.0.to_be_bytes());
		record[START_BLOCK_OFFSET..KEY_BYTES].copy_from_slice(&self.start_block.to_be_bytes());
		let (descriptors, _) = record[KEY_BYTES..].as_chunks_mut::<EXTENT_BYTES>();
		for (bytes, extent) in descriptors.iter_mut().zip(extents) {
			let (start, count) = bytes.split_at_mut(size_of::<u32>());
			start.copy_from_slice(&extent.start.0.to_be_bytes());
			count.copy_from_slice(&extent.blocks.to_be_bytes());
		}
		Ok(record)
	}

	/// Returns the stored-fork identity shared by all its continuation keys.
	#[must_use]
	pub const fn owner(self) -> (CatalogId, ForkKind) {
		(self.id, self.kind)
	}
}

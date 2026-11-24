//! Lossless attribute keys and checked inline values for native HFS+ trees.
//!
//! The modern inline layout and limits are qualified against macOS 14.7.2
//! and the Apple macOS 14.5 SDK declarations for `HFSPlusAttrKey` and
//! `HFSPlusAttrData`.

use core::{cmp::Ordering, mem::size_of};

use crate::{
	Error,
	binary::{put, u16_at, u32_at},
	catalog::{RawName, write_name},
	volume::CatalogId,
};

/// Maximum number of UTF-16 code units in an attribute name.
pub const MAX_NAME_UNITS: usize = 127;
/// Node size of the qualified native attributes tree profile.
pub const NATIVE_NODE_BYTES: u16 = 8192;
/// Inline value size limit observed in the qualified native profile.
pub const NATIVE_INLINE_MAX_VALUE_BYTES: usize = 3802;
/// The attributes tree uses binary name comparison with this header tag.
pub const NATIVE_KEY_COMPARE_TYPE: u8 = 0;

const UTF16_UNIT_BYTES: usize = size_of::<u16>();
const KEY_LENGTH_OFFSET: usize = 0;
const KEY_LENGTH_BYTES: usize = size_of::<u16>();
const KEY_FILE_ID_OFFSET: usize = 4;
const KEY_START_BLOCK_OFFSET: usize = 8;
const KEY_NAME_UNIT_COUNT_OFFSET: usize = 12;
const KEY_NAME_OFFSET: usize = KEY_NAME_UNIT_COUNT_OFFSET + UTF16_UNIT_BYTES;
/// Largest stored key length, excluding the two-byte key length field.
pub const MAX_KEY_LENGTH: usize =
	KEY_NAME_OFFSET - KEY_LENGTH_BYTES + UTF16_UNIT_BYTES * MAX_NAME_UNITS;

const RECORD_KIND_OFFSET: usize = 0;
const INLINE_RECORD: u32 = 0x10;
const INLINE_VALUE_LENGTH_OFFSET: usize = 12;
const INLINE_HEADER_BYTES: usize = INLINE_VALUE_LENGTH_OFFSET + size_of::<u32>();
const INLINE_START_BLOCK: u32 = 0;
const VALUE_ALIGNMENT: usize = 2;
const COMPRESSED_ATTRIBUTE_NAME: &str = "com.apple.decmpfs";

/// Owner ID, raw attribute name, and logical starting block of one record.
/// Attribute records sort by these fields, comparing the name as raw UTF-16.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key<'bytes> {
	/// CNID of the owning catalog object.
	pub file_id: CatalogId,
	/// Raw UTF-16 units without catalog normalization or case folding.
	pub name: RawName<'bytes>,
	/// Starting allocation block index within the attribute's logical value.
	pub start_block: u32,
}

impl<'bytes> Key<'bytes> {
	/// Return the full width of the key, including its length field.
	///
	/// # Errors
	/// Rejects an empty or overlong name.
	pub fn encoded_len(name: &[u16]) -> Result<usize, Error> {
		key_len(name.len())
	}

	/// Encode a key with exactly the provided name units.
	///
	/// # Errors
	/// Rejects invalid name lengths and short destinations before writing.
	pub fn encode(
		file_id: CatalogId,
		name: &[u16],
		start_block: u32,
		destination: &mut [u8],
	) -> Result<usize, Error> {
		let len = Self::encoded_len(name)?;
		let key = destination.get_mut(..len).ok_or(Error::Truncated)?;
		write_key(key, file_id, name, start_block)?;
		Ok(len)
	}

	/// Borrow a key and the remaining leaf or variable-length index record bytes.
	///
	/// # Errors
	/// Rejects invalid name lengths, key lengths, and truncated records.
	pub fn split(bytes: &'bytes [u8]) -> Result<(Self, &'bytes [u8]), Error> {
		let end = usize::from(u16_at(bytes, KEY_LENGTH_OFFSET)?) + KEY_LENGTH_BYTES;
		let units = usize::from(u16_at(bytes, KEY_NAME_UNIT_COUNT_OFFSET)?);
		if end != key_len(units)? {
			return Err(Error::InvalidAttribute);
		}
		let (key, remaining) = bytes.split_at_checked(end).ok_or(Error::Truncated)?;
		let units = key[KEY_NAME_OFFSET..].as_chunks::<UTF16_UNIT_BYTES>().0;
		Ok((
			Self {
				file_id: CatalogId(u32_at(key, KEY_FILE_ID_OFFSET)?),
				name: RawName::from_be_units(units),
				start_block: u32_at(key, KEY_START_BLOCK_OFFSET)?,
			},
			remaining,
		))
	}
}

impl Ord for Key<'_> {
	fn cmp(&self, other: &Self) -> Ordering {
		self.file_id
			.cmp(&other.file_id)
			.then_with(|| self.name.units().cmp(other.name.units()))
			.then_with(|| self.start_block.cmp(&other.start_block))
	}
}

impl PartialOrd for Key<'_> {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

/// One validated attribute leaf record whose value is stored inline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InlineRecord<'bytes> {
	/// Key identifying the owning object and attribute name.
	pub key: Key<'bytes>,
	/// Exactly the declared bytes of the value, excluding record padding.
	pub value: &'bytes [u8],
}

impl<'bytes> InlineRecord<'bytes> {
	/// Return the aligned byte length of a complete inline leaf record.
	///
	/// # Errors
	/// Rejects invalid name lengths, compressed-file attributes, and out-of-profile sizes.
	pub fn encoded_len(name: &[u16], value: &[u8]) -> Result<usize, Error> {
		let (key_bytes, value_bytes) = inline_record_lengths(name, value.len())?;
		Ok(key_bytes + value_bytes)
	}

	/// Write a complete inline record, including zeroed reserved bytes and padding.
	///
	/// # Errors
	/// Rejects invalid names, unsupported sizes or compressed-file attributes,
	/// and short destinations before writing.
	pub fn encode(
		file_id: CatalogId,
		name: &[u16],
		value: &[u8],
		destination: &mut [u8],
	) -> Result<usize, Error> {
		let (key_bytes, value_bytes) = inline_record_lengths(name, value.len())?;
		let len = key_bytes + value_bytes;
		let record = destination.get_mut(..len).ok_or(Error::Truncated)?;
		let (key, inline) = record.split_at_mut(key_bytes);
		write_key(key, file_id, name, INLINE_START_BLOCK)?;
		inline.fill(0);
		put(inline, RECORD_KIND_OFFSET, INLINE_RECORD.to_be_bytes())?;
		put(
			inline,
			INLINE_VALUE_LENGTH_OFFSET,
			(value.len() as u32).to_be_bytes(),
		)?;
		inline[INLINE_HEADER_BYTES..][..value.len()].copy_from_slice(value);
		Ok(len)
	}

	/// Decode the qualified inline profile, borrowing the record's exact value bytes.
	/// Reserved fields and alignment padding do not contribute to the value.
	///
	/// # Errors
	/// Rejects invalid starts or lengths, truncated records, unsupported record
	/// types and sizes, and compressed-file attributes.
	pub fn decode(bytes: &'bytes [u8]) -> Result<Self, Error> {
		let (key, inline) = Key::split(bytes)?;
		let kind = u32_at(inline, RECORD_KIND_OFFSET)?;
		if kind != INLINE_RECORD {
			return Err(Error::UnsupportedAttributeRecord(kind));
		}
		reject_compressed_attribute(key.name.units())?;
		if key.start_block != INLINE_START_BLOCK {
			return Err(Error::InvalidAttribute);
		}
		let size = usize::try_from(u32_at(inline, INLINE_VALUE_LENGTH_OFFSET)?)
			.map_err(|_| Error::InvalidAttribute)?;
		match inline.len().cmp(&inline_len(size)?) {
			Ordering::Less => Err(Error::Truncated),
			Ordering::Greater => Err(Error::InvalidAttribute),
			Ordering::Equal => Ok(Self {
				key,
				value: &inline[INLINE_HEADER_BYTES..][..size],
			}),
		}
	}
}

fn key_len(units: usize) -> Result<usize, Error> {
	if !(1..=MAX_NAME_UNITS).contains(&units) {
		return Err(Error::InvalidAttribute);
	}
	Ok(KEY_NAME_OFFSET + units * UTF16_UNIT_BYTES)
}

fn inline_len(value_bytes: usize) -> Result<usize, Error> {
	if value_bytes > NATIVE_INLINE_MAX_VALUE_BYTES {
		return Err(Error::UnsupportedAttributeSize(value_bytes));
	}
	Ok(INLINE_HEADER_BYTES + value_bytes.next_multiple_of(VALUE_ALIGNMENT))
}

fn inline_record_lengths(name: &[u16], value_bytes: usize) -> Result<(usize, usize), Error> {
	let key_bytes = Key::encoded_len(name)?;
	reject_compressed_attribute(name.iter().copied())?;
	Ok((key_bytes, inline_len(value_bytes)?))
}

fn reject_compressed_attribute(units: impl Iterator<Item = u16>) -> Result<(), Error> {
	if units.eq(COMPRESSED_ATTRIBUTE_NAME.bytes().map(u16::from)) {
		return Err(Error::UnsupportedCompressedAttribute);
	}
	Ok(())
}

fn write_key(
	key: &mut [u8],
	file_id: CatalogId,
	name: &[u16],
	start_block: u32,
) -> Result<(), Error> {
	let stored_len = (key.len() - KEY_LENGTH_BYTES) as u16;
	key.fill(0);
	put(key, KEY_LENGTH_OFFSET, stored_len.to_be_bytes())?;
	put(key, KEY_FILE_ID_OFFSET, file_id.0.to_be_bytes())?;
	put(key, KEY_START_BLOCK_OFFSET, start_block.to_be_bytes())?;
	put(
		key,
		KEY_NAME_UNIT_COUNT_OFFSET,
		(name.len() as u16).to_be_bytes(),
	)?;
	write_name(name, &mut key[KEY_NAME_OFFSET..]);
	Ok(())
}

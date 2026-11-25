//! On-disk journal location and header checks for the HFS+ read-only profile.

use crate::{
	Error,
	binary::{field, u32_at, u64_at},
};

const INTERNAL_INITIALIZED: u32 = 1;
const INFO_FLAGS: usize = 0;
const INFO_OFFSET: usize = 36;
const INFO_SIZE: usize = 44;

/// Bytes through the journal size field in the info block.
pub const INFO_PREFIX_BYTES: usize = INFO_SIZE + size_of::<u64>();

const HEADER_MAGIC: u32 = 0x4a4e_4c78;
const ENDIAN_MARKER: u32 = 0x1234_5678;
const BIG_ENDIAN_MARKER: [u8; 4] = ENDIAN_MARKER.to_be_bytes();
const LITTLE_ENDIAN_MARKER: [u8; 4] = ENDIAN_MARKER.to_le_bytes();
const HEADER_SIGNATURE: usize = 0;
const HEADER_ENDIAN: usize = 4;
const HEADER_START: usize = 8;
const HEADER_END: usize = 16;
const HEADER_SIZE: usize = 24;
const HEADER_BLOCK_LIST_SIZE: usize = 32;
const HEADER_CHECKSUM: usize = 36;
const HEADER_SECTOR_SIZE: usize = 40;
const MIN_JOURNAL_BYTES: u64 = 256 * 1024;
const MAX_JOURNAL_BYTES: u64 = 1024 * 1024 * 1024;
const MIN_SECTOR_BYTES: u64 = 512;
const CHECKSUM_SHIFT: u32 = 8;

/// Bytes of the original journal header covered by the checksum.
///
/// Newer volumes can store a sequence number after this prefix.
pub const HEADER_PREFIX_BYTES: usize = HEADER_SECTOR_SIZE + size_of::<u32>();

/// Location and size of an initialized journal inside the volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info {
	/// Journal header offset in bytes relative to the volume start.
	pub offset: u64,
	/// Length in bytes of the journal header and transaction buffer.
	pub size: u64,
}

impl Info {
	/// Reads the journal info block prefix, whose fields are big-endian.
	///
	/// # Errors
	/// Rejects truncated blocks and any flags outside the initialized internal profile.
	pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
		let bytes = bytes.get(..INFO_PREFIX_BYTES).ok_or(Error::Truncated)?;
		let flags = u32_at(bytes, INFO_FLAGS)?;
		if flags != INTERNAL_INITIALIZED {
			return Err(Error::UnsupportedJournalFlags(flags));
		}
		Ok(Self {
			offset: u64_at(bytes, INFO_OFFSET)?,
			size: u64_at(bytes, INFO_SIZE)?,
		})
	}
}

/// Cursor state of a checksummed journal header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
	/// Both cursors point to the same position and the journal is empty.
	Empty,
	/// The cursors differ and the volume needs replay before logical reads.
	Pending,
}

impl Activity {
	/// Checks the journal header and reads its cursor state.
	///
	/// The checksum covers the original header prefix in its stored byte order,
	/// with the checksum field replaced by zeroes.
	///
	/// # Errors
	/// Rejects a truncated prefix or invalid magic, byte order, checksum or geometry.
	pub fn decode(bytes: &[u8], info: Info) -> Result<Self, Error> {
		let bytes = bytes.get(..HEADER_PREFIX_BYTES).ok_or(Error::Truncated)?;
		let little_endian = match field(bytes, HEADER_ENDIAN)? {
			BIG_ENDIAN_MARKER => false,
			LITTLE_ENDIAN_MARKER => true,
			_ => return Err(Error::InvalidJournalHeader),
		};
		let word = |offset| ordered_field(bytes, offset, little_endian).map(u32::from_be_bytes);
		let wide = |offset| ordered_field(bytes, offset, little_endian).map(u64::from_be_bytes);
		if word(HEADER_SIGNATURE)? != HEADER_MAGIC || word(HEADER_CHECKSUM)? != checksum(bytes) {
			return Err(Error::InvalidJournalHeader);
		}
		let size = wide(HEADER_SIZE)?;
		let sector_bytes = u64::from(word(HEADER_SECTOR_SIZE)?);
		let block_list_bytes = u64::from(word(HEADER_BLOCK_LIST_SIZE)?);
		if size != info.size
			|| !(MIN_JOURNAL_BYTES..=MAX_JOURNAL_BYTES).contains(&size)
			|| sector_bytes < MIN_SECTOR_BYTES
			|| !sector_bytes.is_power_of_two()
			|| !info.offset.is_multiple_of(sector_bytes)
			|| !size.is_multiple_of(sector_bytes)
			|| block_list_bytes < sector_bytes
			|| !block_list_bytes.is_multiple_of(MIN_SECTOR_BYTES)
			|| block_list_bytes + sector_bytes >= size
		{
			return Err(Error::InvalidJournalHeader);
		}
		let start = wide(HEADER_START)?;
		let end = wide(HEADER_END)?;
		for cursor in [start, end] {
			// Apple's reader allows the end-of-ring boundary and validates
			// 512-byte alignment even on media with larger logical sectors.
			if !(sector_bytes..=size).contains(&cursor) || !cursor.is_multiple_of(MIN_SECTOR_BYTES)
			{
				return Err(Error::InvalidJournalHeader);
			}
		}
		Ok(if start == end {
			Self::Empty
		} else {
			Self::Pending
		})
	}
}

fn ordered_field<const N: usize>(
	bytes: &[u8],
	offset: usize,
	little_endian: bool,
) -> Result<[u8; N], Error> {
	let mut value = field(bytes, offset)?;
	if little_endian {
		value.reverse();
	}
	Ok(value)
}

fn checksum(bytes: &[u8]) -> u32 {
	let start = &bytes[..HEADER_CHECKSUM];
	let end = &bytes[HEADER_CHECKSUM + size_of::<u32>()..];
	let bytes = start
		.iter()
		.copied()
		.chain(0u32.to_ne_bytes())
		.chain(end.iter().copied());
	!bytes.fold(0u32, |sum, byte| {
		(sum << CHECKSUM_SHIFT) ^ sum.wrapping_add(u32::from(byte))
	})
}

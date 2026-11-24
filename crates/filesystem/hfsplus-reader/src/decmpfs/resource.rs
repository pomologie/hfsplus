use alloc::vec::Vec;

use hfsplus_core::{ByteLen, ByteOffset, ReadAt};

use super::{
	CompressionLimit, LOGICAL_CHUNK_BYTES, LogicalDataError, ResolvedFork, codec::Decoder,
};

const RESOURCE_HEADER_BYTES: usize = 4 * size_of::<u32>();
const RESOURCE_MAP_BYTES: usize = 50;
const TYPE_LIST_OFFSET_FIELD: usize = 24;
const NAME_LIST_OFFSET_FIELD: usize = 26;
const COMPRESSION_TYPE_LIST: u16 = 28;
const TYPE_COUNT_FIELD: usize = COMPRESSION_TYPE_LIST as usize;
const TYPE_CODE_FIELD: usize = TYPE_COUNT_FIELD + size_of::<u16>();
const TYPE_REFERENCE_COUNT_FIELD: usize = TYPE_CODE_FIELD + 4;
const REFERENCES_OFFSET_FIELD: usize = TYPE_REFERENCE_COUNT_FIELD + size_of::<u16>();
const COMPRESSION_REFERENCE_OFFSET: u16 = 10;
const REFERENCE_ID_FIELD: usize =
	COMPRESSION_TYPE_LIST as usize + COMPRESSION_REFERENCE_OFFSET as usize;
const REFERENCE_NAME_FIELD: usize = REFERENCE_ID_FIELD + size_of::<u16>();
const REFERENCE_DATA_FIELD: usize = REFERENCE_NAME_FIELD + size_of::<u16>();
const REFERENCE_OFFSET_MASK: u32 = 0x00ff_ffff;
const COMPRESSED_RESOURCE_TYPE: &[u8; 4] = b"cmpf";
const COMPRESSED_RESOURCE_ID: u16 = 1;
const UNNAMED_RESOURCE: u16 = u16::MAX;
const SINGLE_ENTRY_COUNT: u16 = 0; // Resource Manager counts store count minus one.

const RESOURCE_LENGTH_BYTES: usize = size_of::<u32>();
const BLOCK_COUNT_BYTES: usize = size_of::<u32>();
const TABLE_ENTRY_BYTES: usize = 2 * size_of::<u32>();
const MAX_STORED_FRAME_BYTES: u32 = LOGICAL_CHUNK_BYTES as u32 + 1;

#[derive(Debug)]
struct Frame {
	start: ByteOffset,
	stored_bytes: usize,
}

#[derive(Debug)]
pub(super) struct ResourceData<'file, S> {
	fork: ResolvedFork<'file, S>,
	expanded: ByteLen,
	frames: Vec<Frame>,
}

impl<'file, S: ReadAt> ResourceData<'file, S> {
	pub(super) fn open(
		fork: ResolvedFork<'file, S>,
		expanded: ByteLen,
		max_chunks: usize,
	) -> Result<Self, LogicalDataError<S::Error>> {
		let count = expanded.0.div_ceil(LOGICAL_CHUNK_BYTES as u64);
		let count = usize::try_from(count)
			.map_err(|_| LogicalDataError::Limit(CompressionLimit::ResourceChunks))?;
		if count > max_chunks {
			return Err(LogicalDataError::Limit(CompressionLimit::ResourceChunks));
		}
		if count == 0 {
			return Err(LogicalDataError::ResourceLayout);
		}

		let (offset, length) = resource_data(&fork)?;
		let mut prefix = [0; RESOURCE_LENGTH_BYTES + BLOCK_COUNT_BYTES];
		fork.read_exact_at(ByteOffset(offset), &mut prefix)
			.map_err(LogicalDataError::Read)?;
		let declared_length =
			u64::from(read_be_u32(&prefix, 0).ok_or(LogicalDataError::ResourceLayout)?);
		let declared_count =
			read_le_u32(&prefix, RESOURCE_LENGTH_BYTES).ok_or(LogicalDataError::ResourceLayout)?;
		let count_u32 = u32::try_from(count).map_err(|_| LogicalDataError::ResourceLayout)?;
		let body = offset + RESOURCE_LENGTH_BYTES as u64;
		let table = body + BLOCK_COUNT_BYTES as u64;
		if declared_length + RESOURCE_LENGTH_BYTES as u64 != length || declared_count != count_u32 {
			return Err(LogicalDataError::ResourceLayout);
		}
		let table_bytes = count
			.checked_mul(TABLE_ENTRY_BYTES)
			.ok_or(LogicalDataError::ResourceLayout)?;
		let mut next_frame = BLOCK_COUNT_BYTES as u64 + table_bytes as u64;
		if next_frame >= declared_length {
			return Err(LogicalDataError::ResourceLayout);
		}

		let mut entries = Vec::new();
		reserve(&mut entries, table_bytes)?;
		fork.read_exact_at(ByteOffset(table), &mut entries)
			.map_err(LogicalDataError::Read)?;
		let mut frames = Vec::new();
		frames.try_reserve_exact(count)?;
		for entry in entries.as_chunks::<TABLE_ENTRY_BYTES>().0 {
			let relative =
				u64::from(read_le_u32(entry, 0).ok_or(LogicalDataError::ResourceLayout)?);
			let stored_bytes =
				read_le_u32(entry, size_of::<u32>()).ok_or(LogicalDataError::ResourceLayout)?;
			if relative != next_frame || !(1..=MAX_STORED_FRAME_BYTES).contains(&stored_bytes) {
				return Err(LogicalDataError::ResourceLayout);
			}
			next_frame = relative + u64::from(stored_bytes);
			if next_frame > declared_length {
				return Err(LogicalDataError::ResourceLayout);
			}
			frames.push(Frame {
				start: ByteOffset(body + relative),
				stored_bytes: stored_bytes as usize,
			});
		}
		if next_frame != declared_length {
			return Err(LogicalDataError::ResourceLayout);
		}
		Ok(Self {
			fork,
			expanded,
			frames,
		})
	}

	pub(super) const fn len(&self) -> ByteLen {
		self.expanded
	}

	pub(super) fn read_exact_at(
		&self,
		offset: ByteOffset,
		mut destination: &mut [u8],
	) -> Result<(), LogicalDataError<S::Error>> {
		let mut cursor = offset.0;
		let mut encoded = Vec::new();
		let mut decoded = Vec::new();
		let mut decoder = Decoder::default();
		while !destination.is_empty() {
			let index = cursor / LOGICAL_CHUNK_BYTES as u64;
			let ordinal = usize::try_from(index).map_err(|_| LogicalDataError::ResourceLayout)?;
			let frame = self
				.frames
				.get(ordinal)
				.ok_or(LogicalDataError::ResourceLayout)?;
			let start = index * LOGICAL_CHUNK_BYTES as u64;
			let size = (self.expanded.0 - start).min(LOGICAL_CHUNK_BYTES as u64) as usize;
			let stored = reserve(&mut encoded, frame.stored_bytes)?;
			let output = reserve(&mut decoded, size)?;
			self.fork
				.read_exact_at(frame.start, stored)
				.map_err(LogicalDataError::Read)?;
			decoder
				.decode(stored, output)
				.map_err(|error| LogicalDataError::Frame {
					block: index,
					error,
				})?;

			let relative = (cursor - start) as usize;
			let count = destination.len().min(size - relative);
			let (target, rest) = destination.split_at_mut(count);
			target.copy_from_slice(&output[relative..relative + count]);
			cursor += count as u64;
			destination = rest;
		}
		Ok(())
	}
}

fn resource_data<S: ReadAt>(
	fork: &ResolvedFork<'_, S>,
) -> Result<(u64, u64), LogicalDataError<S::Error>> {
	let mut header = [0; RESOURCE_HEADER_BYTES];
	if fork.len().0 < RESOURCE_HEADER_BYTES as u64 {
		return Err(LogicalDataError::ResourceLayout);
	}
	fork.read_exact_at(ByteOffset(0), &mut header)
		.map_err(LogicalDataError::Read)?;
	let [data, map, data_bytes, map_bytes] = header.as_chunks::<{ size_of::<u32>() }>().0 else {
		return Err(LogicalDataError::ResourceLayout);
	};
	let data = u64::from(u32::from_be_bytes(*data));
	let map = u64::from(u32::from_be_bytes(*map));
	let data_bytes = u64::from(u32::from_be_bytes(*data_bytes));
	let map_bytes = u64::from(u32::from_be_bytes(*map_bytes));
	if data < RESOURCE_HEADER_BYTES as u64
		|| data_bytes < (RESOURCE_LENGTH_BYTES + BLOCK_COUNT_BYTES) as u64
		|| data + data_bytes != map
		|| map_bytes != RESOURCE_MAP_BYTES as u64
		|| map + map_bytes != fork.len().0
	{
		return Err(LogicalDataError::ResourceLayout);
	}
	let mut directory = [0; RESOURCE_MAP_BYTES];
	fork.read_exact_at(ByteOffset(map), &mut directory)
		.map_err(LogicalDataError::Read)?;
	if !valid_map(&directory) {
		return Err(LogicalDataError::ResourceLayout);
	}
	Ok((data, data_bytes))
}

fn valid_map(map: &[u8; RESOURCE_MAP_BYTES]) -> bool {
	read_be_u16(map, TYPE_LIST_OFFSET_FIELD) == Some(COMPRESSION_TYPE_LIST)
		&& read_be_u16(map, NAME_LIST_OFFSET_FIELD) == Some(RESOURCE_MAP_BYTES as u16)
		&& read_be_u16(map, TYPE_COUNT_FIELD) == Some(SINGLE_ENTRY_COUNT)
		&& map.get(TYPE_CODE_FIELD..TYPE_REFERENCE_COUNT_FIELD) == Some(COMPRESSED_RESOURCE_TYPE)
		&& read_be_u16(map, TYPE_REFERENCE_COUNT_FIELD) == Some(SINGLE_ENTRY_COUNT)
		&& read_be_u16(map, REFERENCES_OFFSET_FIELD) == Some(COMPRESSION_REFERENCE_OFFSET)
		&& read_be_u16(map, REFERENCE_ID_FIELD) == Some(COMPRESSED_RESOURCE_ID)
		&& read_be_u16(map, REFERENCE_NAME_FIELD) == Some(UNNAMED_RESOURCE)
		&& read_be_u32(map, REFERENCE_DATA_FIELD)
			.is_some_and(|field| field & REFERENCE_OFFSET_MASK == 0)
}

fn reserve<E>(bytes: &mut Vec<u8>, len: usize) -> Result<&mut [u8], LogicalDataError<E>> {
	if let Some(additional) = len.checked_sub(bytes.len()) {
		bytes.try_reserve_exact(additional)?;
		bytes.resize(len, 0);
	}
	Ok(&mut bytes[..len])
}

fn read_be_u16(bytes: &[u8], at: usize) -> Option<u16> {
	Some(u16::from_be_bytes(*bytes.get(at..)?.first_chunk()?))
}

fn read_be_u32(bytes: &[u8], at: usize) -> Option<u32> {
	Some(u32::from_be_bytes(*bytes.get(at..)?.first_chunk()?))
}

fn read_le_u32(bytes: &[u8], at: usize) -> Option<u32> {
	Some(u32::from_le_bytes(*bytes.get(at..)?.first_chunk()?))
}

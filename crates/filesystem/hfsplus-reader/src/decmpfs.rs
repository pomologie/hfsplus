//! Logical file data on clean HFS+ and HFSX volumes.
//!
//! Ordinary files use their data forks. The optional `decmpfs-zlib` feature
//! admits inline zlib and chunked zlib resource forks. Raw forks remain
//! available through [`RegularFile::open_stored_fork`].
//!
//! The qualified resource profile uses one unnamed `cmpf` resource, ordered
//! 64 KiB logical blocks, and zlib or exact stored frames. The expanded length
//! and resource table count are admitted before allocation. The native H-READ-03
//! fixtures cover inline and resource forks, a stored partial final block, and
//! compressed hard links with their original attribute owner.
//!
//! The opt-in codec pins `miniz_oxide` 0.9.1 (MIT, Zlib, or Apache-2.0) with
//! defaults disabled. The only additional normal dependency is `adler2` 2.0.1
//! (0BSD, MIT, or Apache-2.0). Both bare-metal profiles use the same zlib checks.

use core::fmt;

use hfsplus_core::{ByteLen, ByteOffset, ByteRange, RangeError, ReadAt};

use crate::{
	Error as VolumeError,
	attributes::AttributeReadLimits,
	catalog::RegularFile,
	overflow::{ForkKind, OverflowError, OverflowLimits, ResolvedFork},
};

#[cfg(feature = "decmpfs-zlib")]
use alloc::vec::Vec;

#[cfg(feature = "decmpfs-zlib")]
mod codec;
#[cfg(feature = "decmpfs-zlib")]
mod resource;

#[cfg(feature = "decmpfs-zlib")]
pub use codec::FrameError;

const BSD_COMPRESSED: u8 = 1 << 5;
const DEFAULT_MAX_EXPANDED_BYTES: u64 = 1 << 30;
const DEFAULT_MAX_RESOURCE_CHUNKS: usize = 16_384;

#[cfg(feature = "decmpfs-zlib")]
const MAGIC: &[u8; 4] = b"fpmc";
#[cfg(feature = "decmpfs-zlib")]
const ALGORITHM_OFFSET: usize = MAGIC.len();
#[cfg(feature = "decmpfs-zlib")]
const SIZE_OFFSET: usize = ALGORITHM_OFFSET + size_of::<u32>();
#[cfg(feature = "decmpfs-zlib")]
const METADATA_HEADER_BYTES: usize = SIZE_OFFSET + size_of::<u64>();
#[cfg(feature = "decmpfs-zlib")]
const ZLIB_INLINE: u32 = 3;
#[cfg(feature = "decmpfs-zlib")]
const ZLIB_RESOURCE: u32 = 4;
#[cfg(feature = "decmpfs-zlib")]
const LOGICAL_CHUNK_BYTES: usize = 64 * 1024;

/// Limits for one logical data view.
#[derive(Debug, Clone, Copy)]
pub struct LogicalDataLimits {
	/// Lookup limits for the file's compression attribute.
	pub attributes: AttributeReadLimits,
	/// Overflow lookup limits for a stored fork used by the reader.
	pub overflow: OverflowLimits,
	/// Largest admitted decoded size for a compressed file, in bytes.
	pub max_expanded_bytes: ByteLen,
	/// Largest admitted block table for resource-backed compression.
	pub max_resource_chunks: usize,
}

impl Default for LogicalDataLimits {
	fn default() -> Self {
		Self {
			attributes: AttributeReadLimits::default(),
			overflow: OverflowLimits::default(),
			max_expanded_bytes: ByteLen(DEFAULT_MAX_EXPANDED_BYTES),
			max_resource_chunks: DEFAULT_MAX_RESOURCE_CHUNKS,
		}
	}
}

/// The checked size limit exceeded by a compressed file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CompressionLimit {
	/// Caller-defined logical expansion budget.
	ExpandedBytes,
	/// The inline zlib profile admits at most one decoded 64 KiB block.
	InlineBytes,
	/// Caller-defined maximum resource-fork block count.
	ResourceChunks,
}

/// Logical reads failed before the requested bytes could be returned.
#[derive(Debug)]
#[non_exhaustive]
pub enum LogicalDataError<E> {
	/// The logical read extends outside the data view.
	Bounds(RangeError),
	/// The file is compressed and this build excludes the decmpfs zlib decoder.
	CompressionDisabled,
	/// The BSD compression flag is set and compression metadata is absent.
	MissingCompressionMetadata,
	/// A stored fork contradicts the advertised compressed representation.
	ConflictingForks,
	/// The file's compression type is outside the qualified profile.
	UnsupportedCompression(u32),
	/// The compression attribute has a malformed header or payload layout.
	CompressionMetadata,
	/// The compression resource header, map, or block table is inconsistent.
	ResourceLayout,
	/// The operation exceeded an expansion or metadata limit.
	Limit(CompressionLimit),
	/// An error opening the underlying stored fork.
	Fork(OverflowError<E>),
	/// A failure reading the underlying stored bytes.
	Read(VolumeError<E>),
	/// The compression attribute could not be read.
	#[cfg(feature = "decmpfs-zlib")]
	Attributes(crate::attributes::AttributeReadError<E>),
	/// The requested compressed block is malformed.
	#[cfg(feature = "decmpfs-zlib")]
	Frame {
		/// Logical block index within this file. Inline uses block zero.
		block: u64,
		/// Framing or decoder failure.
		error: FrameError,
	},
	/// Working memory could not be reserved within the admitted limits.
	#[cfg(feature = "decmpfs-zlib")]
	Allocation(alloc::collections::TryReserveError),
}

impl<E: fmt::Display> fmt::Display for LogicalDataError<E> {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Bounds(error) => error.fmt(formatter),
			Self::CompressionDisabled => formatter.write_str("decmpfs zlib support is disabled"),
			Self::MissingCompressionMetadata => {
				formatter.write_str("compressed file is missing its decmpfs attribute")
			}
			Self::ConflictingForks => {
				formatter.write_str("stored forks contradict the decmpfs representation")
			}
			Self::UnsupportedCompression(kind) => {
				write!(formatter, "unsupported decmpfs compression type {kind}")
			}
			Self::CompressionMetadata => formatter.write_str("malformed decmpfs attribute"),
			Self::ResourceLayout => formatter.write_str("malformed decmpfs resource fork layout"),
			Self::Limit(limit) => write!(formatter, "decmpfs limit exceeded: {limit:?}"),
			Self::Fork(error) => error.fmt(formatter),
			Self::Read(error) => error.fmt(formatter),
			#[cfg(feature = "decmpfs-zlib")]
			Self::Attributes(error) => error.fmt(formatter),
			#[cfg(feature = "decmpfs-zlib")]
			Self::Frame { block, error } => write!(formatter, "decmpfs block {block}: {error}"),
			#[cfg(feature = "decmpfs-zlib")]
			Self::Allocation(error) => error.fmt(formatter),
		}
	}
}

impl<E: core::error::Error + 'static> core::error::Error for LogicalDataError<E> {
	fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
		match self {
			Self::Bounds(error) => Some(error),
			Self::Fork(error) => Some(error),
			Self::Read(error) => Some(error),
			#[cfg(feature = "decmpfs-zlib")]
			Self::Attributes(error) => Some(error),
			#[cfg(feature = "decmpfs-zlib")]
			Self::Frame { error, .. } => Some(error),
			#[cfg(feature = "decmpfs-zlib")]
			Self::Allocation(error) => Some(error),
			_ => None,
		}
	}
}

#[cfg(feature = "decmpfs-zlib")]
impl<E> From<alloc::collections::TryReserveError> for LogicalDataError<E> {
	fn from(error: alloc::collections::TryReserveError) -> Self {
		Self::Allocation(error)
	}
}

/// An immutable positional view of one file's logical data bytes.
#[derive(Debug)]
pub struct LogicalData<'file, S> {
	content: Content<'file, S>,
}

#[derive(Debug)]
enum Content<'file, S> {
	Stored(ResolvedFork<'file, S>),
	#[cfg(feature = "decmpfs-zlib")]
	Inline(Vec<u8>),
	#[cfg(feature = "decmpfs-zlib")]
	Resource(resource::ResourceData<'file, S>),
}

impl<S: ReadAt> RegularFile<'_, S> {
	/// Opens logical file data using the resolved inode's BSD flags, compression
	/// metadata, and stored forks. Resource-backed data is decoded per requested
	/// block. Each decoded block must consume its entire admitted frame.
	///
	/// # Errors
	/// Returns typed errors for unavailable compression, corrupt metadata,
	/// unrepresentable lengths, exhausted limits, or a failed source read.
	pub fn open_logical_data(
		&self,
		limits: LogicalDataLimits,
	) -> Result<LogicalData<'_, S>, LogicalDataError<S::Error>> {
		let content = if self.metadata().permissions.owner_flags & BSD_COMPRESSED == 0 {
			Content::Stored(
				self.open_stored_fork(ForkKind::Data, limits.overflow)
					.map_err(LogicalDataError::Fork)?,
			)
		} else {
			#[cfg(feature = "decmpfs-zlib")]
			{
				open_compressed(self, limits)?
			}
			#[cfg(not(feature = "decmpfs-zlib"))]
			return Err(LogicalDataError::CompressionDisabled);
		};
		Ok(LogicalData { content })
	}
}

impl<S: ReadAt> ReadAt for LogicalData<'_, S> {
	type Error = LogicalDataError<S::Error>;

	fn len(&self) -> ByteLen {
		match &self.content {
			Content::Stored(reader) => reader.len(),
			#[cfg(feature = "decmpfs-zlib")]
			Content::Inline(bytes) => ByteLen::from(bytes.len()),
			#[cfg(feature = "decmpfs-zlib")]
			Content::Resource(reader) => reader.len(),
		}
	}

	fn read_exact_at(&self, offset: ByteOffset, destination: &mut [u8]) -> Result<(), Self::Error> {
		ByteRange::new(offset, destination.len().into(), self.len())
			.map_err(LogicalDataError::Bounds)?;
		match &self.content {
			Content::Stored(reader) => reader
				.read_exact_at(offset, destination)
				.map_err(LogicalDataError::Read),
			#[cfg(feature = "decmpfs-zlib")]
			Content::Inline(bytes) => bytes
				.as_slice()
				.read_exact_at(offset, destination)
				.map_err(LogicalDataError::Bounds),
			#[cfg(feature = "decmpfs-zlib")]
			Content::Resource(reader) => reader.read_exact_at(offset, destination),
		}
	}
}

#[cfg(feature = "decmpfs-zlib")]
fn open_compressed<'file, S: ReadAt>(
	file: &'file RegularFile<'_, S>,
	limits: LogicalDataLimits,
) -> Result<Content<'file, S>, LogicalDataError<S::Error>> {
	let attributes = file
		.volume()
		.attributes(limits.attributes)
		.map_err(LogicalDataError::Attributes)?;
	let metadata = attributes
		.read_compression_metadata(file.inode_id())
		.map_err(LogicalDataError::Attributes)?
		.ok_or(LogicalDataError::MissingCompressionMetadata)?;
	let (header, payload) = metadata
		.split_at_checked(METADATA_HEADER_BYTES)
		.ok_or(LogicalDataError::CompressionMetadata)?;
	if !header.starts_with(MAGIC) {
		return Err(LogicalDataError::CompressionMetadata);
	}
	let algorithm = u32::from_le_bytes(
		header[ALGORITHM_OFFSET..SIZE_OFFSET]
			.try_into()
			.map_err(|_| LogicalDataError::CompressionMetadata)?,
	);
	let expanded = u64::from_le_bytes(
		header[SIZE_OFFSET..]
			.try_into()
			.map_err(|_| LogicalDataError::CompressionMetadata)?,
	);
	if !matches!(algorithm, ZLIB_INLINE | ZLIB_RESOURCE) {
		return Err(LogicalDataError::UnsupportedCompression(algorithm));
	}
	if expanded > limits.max_expanded_bytes.0 {
		return Err(LogicalDataError::Limit(CompressionLimit::ExpandedBytes));
	}
	if !file
		.open_stored_fork(ForkKind::Data, limits.overflow)
		.map_err(LogicalDataError::Fork)?
		.is_empty()
	{
		return Err(LogicalDataError::ConflictingForks);
	}
	if algorithm == ZLIB_INLINE {
		if expanded > LOGICAL_CHUNK_BYTES as u64 {
			return Err(LogicalDataError::Limit(CompressionLimit::InlineBytes));
		}
		let mut output = Vec::new();
		output.try_reserve_exact(expanded as usize)?;
		output.resize(expanded as usize, 0);
		codec::Decoder::default()
			.decode(payload, &mut output)
			.map_err(|error| LogicalDataError::Frame { block: 0, error })?;
		return Ok(Content::Inline(output));
	}
	if !payload.is_empty() {
		return Err(LogicalDataError::CompressionMetadata);
	}
	let fork = file
		.open_stored_fork(ForkKind::Resource, limits.overflow)
		.map_err(LogicalDataError::Fork)?;
	if fork.is_empty() {
		return Err(LogicalDataError::ConflictingForks);
	}
	Ok(Content::Resource(resource::ResourceData::open(
		fork,
		ByteLen(expanded),
		limits.max_resource_chunks,
	)?))
}

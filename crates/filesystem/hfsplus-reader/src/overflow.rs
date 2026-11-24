//! Stored-fork reads through the HFS+ extents overflow tree.

use alloc::{collections::TryReserveError, vec::Vec};
use core::fmt;

use hfsplus_core::{ByteLen, ByteOffset, ByteRange, ReadAt};
use hfsplus_format::{
	Error as FormatError,
	volume::{AllocationBlock, CatalogId, Extent, Fork, Geometry},
};

use crate::{Error, ForkReadSupport, ForkReader, SpecialFile, Volume};

mod tree;

const CATALOG_ID: CatalogId = CatalogId(4);
const EXTENTS_ID: CatalogId = CatalogId(3);
const ALLOCATION_ID: CatalogId = CatalogId(6);
const STARTUP_ID: CatalogId = CatalogId(7);
const ATTRIBUTES_ID: CatalogId = CatalogId(8);

const DEFAULT_NODE_BYTES: u16 = 32_768;
const DEFAULT_METADATA_READS: usize = 4096;
const DEFAULT_OVERFLOW_RECORDS: usize = 4096;

pub(super) const EXTENTS_PER_RECORD: usize = 8;
const EXTENT_BYTES: usize = size_of::<u32>() * 2;
pub(super) const RECORD_VALUE_BYTES: usize = EXTENTS_PER_RECORD * EXTENT_BYTES;

/// Which stored fork an overflow key identifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum ForkKind {
	/// Data fork.
	Data = 0,
	/// Resource fork.
	Resource = 0xff,
}

/// Resource limits for resolving all allocated blocks in one fork.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverflowLimits {
	/// Maximum bytes in an overflow-tree node and its scratch buffer.
	pub max_node_bytes: u16,
	/// Maximum logical metadata reads, including the header probe, header node,
	/// routed nodes, allocation map nodes and neighboring leaves.
	pub max_metadata_reads: usize,
	/// Maximum continuation records for this fork, excluding embedded extents.
	pub max_records: usize,
}

impl Default for OverflowLimits {
	fn default() -> Self {
		Self {
			max_node_bytes: DEFAULT_NODE_BYTES,
			max_metadata_reads: DEFAULT_METADATA_READS,
			max_records: DEFAULT_OVERFLOW_RECORDS,
		}
	}
}

/// Which bound stopped overflow resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OverflowLimit {
	/// Node size.
	NodeBytes,
	/// Number of metadata reads through the overflow fork.
	MetadataReads,
	/// Number of continuation records for the requested fork.
	Records,
}

/// An error opening a fork that may require the extents overflow tree.
#[derive(Debug)]
#[non_exhaustive]
pub enum OverflowError<E> {
	/// The volume, embedded fork or tree could not be read or decoded.
	Read(Error<E>),
	/// The extents overflow file requires extents beyond its embedded descriptor.
	BootstrapOverflow,
	/// A required record is absent at this logical allocation block.
	MissingRecord { start_block: u32 },
	/// A continuation enters the chain at the wrong logical allocation block.
	InvalidStart {
		/// Next block after the previous group.
		expected: u32,
		/// Block at which the new continuation begins.
		actual: u32,
	},
	/// A caller-specified traversal bound was exhausted.
	Limit(OverflowLimit),
	/// A bounded allocation could not be reserved.
	Allocation(TryReserveError),
}

impl<E: fmt::Display> fmt::Display for OverflowError<E> {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Read(error) => error.fmt(formatter),
			Self::BootstrapOverflow => {
				formatter.write_str("extents overflow file cannot bootstrap its own extents")
			}
			Self::MissingRecord { start_block } => {
				write!(
					formatter,
					"missing overflow extent record at fork block {start_block}"
				)
			}
			Self::InvalidStart { expected, actual } => write!(
				formatter,
				"overflow chain expected fork block {expected} and found {actual}"
			),
			Self::Limit(limit) => write!(formatter, "extent resolution limit exceeded: {limit:?}"),
			Self::Allocation(error) => write!(formatter, "cannot reserve extent metadata: {error}"),
		}
	}
}

impl<E: core::error::Error + 'static> core::error::Error for OverflowError<E> {
	fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
		match self {
			Self::Read(error) => Some(error),
			Self::Allocation(error) => Some(error),
			_ => None,
		}
	}
}

impl<E> From<Error<E>> for OverflowError<E> {
	fn from(error: Error<E>) -> Self {
		Self::Read(error)
	}
}

impl<E> From<FormatError> for OverflowError<E> {
	fn from(error: FormatError) -> Self {
		Self::Read(Error::Format(error))
	}
}

impl<E> From<TryReserveError> for OverflowError<E> {
	fn from(error: TryReserveError) -> Self {
		Self::Allocation(error)
	}
}

impl<S: ReadAt> Volume<S> {
	/// Opens a stored fork using its embedded extents and any exact continuation
	/// chain keyed by the supplied catalog ID and fork kind.
	///
	/// The caller supplies a descriptor from this volume. The reader validates
	/// the resolved physical extents within the fork, leaving allocation bitmap
	/// and cross-fork ownership verification to a filesystem checker.
	///
	/// # Errors
	/// Fails for an unsupported volume, malformed or incomplete allocation
	/// chain, unsupported tree bootstrap, source failure, or exhausted limits.
	pub fn open_fork(
		&self,
		id: CatalogId,
		kind: ForkKind,
		fork: &Fork,
		limits: OverflowLimits,
	) -> Result<ResolvedFork<'_, S>, OverflowError<S::Error>> {
		let logical_len = ByteLen(fork.logical_bytes);
		if !fork.has_overflow() {
			return Ok(ResolvedFork {
				mapping: Mapping::Embedded(self.open_embedded_fork(fork)?),
				logical_len,
			});
		}

		let support = self.fork_read_support();
		if support != ForkReadSupport::Supported {
			return Err(Error::UnsupportedVolume(support).into());
		}
		if id == EXTENTS_ID || self.header().forks.extents.has_overflow() {
			return Err(OverflowError::BootstrapOverflow);
		}
		if limits.max_records == 0 {
			return Err(OverflowError::Limit(OverflowLimit::Records));
		}
		let geometry = self.header().geometry;
		fork.validate_geometry(geometry)?;
		let mut groups = Vec::new();
		let mut covered = fork.extents().map(|extent| extent.blocks).sum();
		let mut embedded_extents = [ZERO_EXTENT; EXTENTS_PER_RECORD];
		for (slot, extent) in embedded_extents.iter_mut().zip(fork.extents()) {
			*slot = extent;
		}
		let embedded = Fork::new(
			group_bytes(covered, geometry),
			0,
			covered,
			&embedded_extents,
			geometry,
		)?;
		push_group(self, &mut groups, embedded)?;

		let extents = self.open_special_fork(SpecialFile::Extents)?;
		if extents.is_empty() {
			return Err(OverflowError::MissingRecord {
				start_block: covered,
			});
		}
		let mut count = 0;
		let mut overflow = tree::OverflowTree::open(extents, limits)?;
		overflow.walk(id, kind, |start_block, value| {
			if covered == fork.total_blocks {
				return Err(FormatError::InvalidFork.into());
			}
			if start_block != covered {
				return Err(OverflowError::InvalidStart {
					expected: covered,
					actual: start_block,
				});
			}
			if count >= limits.max_records {
				return Err(OverflowError::Limit(OverflowLimit::Records));
			}
			let (group, used_extents) = decode_group(value, geometry)?;
			covered = covered
				.checked_add(group.total_blocks)
				.filter(|&blocks| blocks <= fork.total_blocks)
				.ok_or(FormatError::InvalidFork)?;
			if covered < fork.total_blocks && used_extents != EXTENTS_PER_RECORD {
				return Err(FormatError::InvalidFork.into());
			}
			push_group(self, &mut groups, group)?;
			count += 1;
			Ok(())
		})?;
		if covered != fork.total_blocks {
			return Err(OverflowError::MissingRecord {
				start_block: covered,
			});
		}
		check_disjoint(&groups)?;
		Ok(ResolvedFork {
			mapping: Mapping::Overflow {
				volume: self,
				groups,
			},
			logical_len,
		})
	}

	/// Opens a special file, resolving ordinary overflow chains when needed.
	/// The extents overflow file itself must fit within its embedded extents.
	///
	/// # Errors
	/// Fails if the fork cannot be opened within the supplied limits and profile.
	pub fn open_resolved_special_fork(
		&self,
		file: SpecialFile,
		limits: OverflowLimits,
	) -> Result<ResolvedFork<'_, S>, OverflowError<S::Error>> {
		let forks = &self.header().forks;
		let (id, fork) = match file {
			SpecialFile::Allocation => (ALLOCATION_ID, &forks.allocation),
			SpecialFile::Extents => (EXTENTS_ID, &forks.extents),
			SpecialFile::Catalog => (CATALOG_ID, &forks.catalog),
			SpecialFile::Attributes => (ATTRIBUTES_ID, &forks.attributes),
			SpecialFile::Startup => (STARTUP_ID, &forks.startup),
		};
		self.open_fork(id, ForkKind::Data, fork, limits)
	}
}

/// Bounded positional reads over a fork's stored bytes in logical order.
#[derive(Debug)]
pub struct ResolvedFork<'a, S> {
	mapping: Mapping<'a, S>,
	logical_len: ByteLen,
}

#[derive(Debug)]
enum Mapping<'a, S> {
	Embedded(ForkReader<'a, S>),
	Overflow {
		volume: &'a Volume<S>,
		groups: Vec<Fork>,
	},
}

impl<S> ResolvedFork<'_, S> {
	/// Physical allocation extents in stored-fork logical order, including slack.
	pub fn extents(&self) -> impl Iterator<Item = Extent> + '_ {
		let groups = match &self.mapping {
			Mapping::Embedded(reader) => core::slice::from_ref(reader.descriptor()),
			Mapping::Overflow { groups, .. } => groups.as_slice(),
		};
		groups.iter().flat_map(Fork::extents)
	}
}

impl<S: ReadAt> ReadAt for ResolvedFork<'_, S> {
	type Error = Error<S::Error>;

	fn len(&self) -> ByteLen {
		self.logical_len
	}

	fn read_exact_at(
		&self,
		offset: ByteOffset,
		mut destination: &mut [u8],
	) -> Result<(), Self::Error> {
		ByteRange::new(offset, destination.len().into(), self.len()).map_err(Error::Bounds)?;
		let (volume, groups) = match &self.mapping {
			Mapping::Embedded(reader) => return reader.read_exact_at(offset, destination),
			Mapping::Overflow { volume, groups } => (volume, groups),
		};

		let mut relative = offset.0;
		for group in groups {
			if destination.is_empty() {
				break;
			}
			if relative >= group.logical_bytes {
				relative -= group.logical_bytes;
				continue;
			}
			let available = group.logical_bytes - relative;
			let count = destination
				.len()
				.min(usize::try_from(available).unwrap_or(usize::MAX));
			let (chunk, remaining) = destination.split_at_mut(count);
			volume
				.open_embedded_fork(group)?
				.read_exact_at(ByteOffset(relative), chunk)?;
			destination = remaining;
			relative = 0;
		}
		if destination.is_empty() {
			Ok(())
		} else {
			Err(Error::Format(FormatError::InvalidFork))
		}
	}
}

const ZERO_EXTENT: Extent = Extent {
	start: AllocationBlock(0),
	blocks: 0,
};

fn group_bytes(blocks: u32, geometry: Geometry) -> u64 {
	u64::from(blocks) * u64::from(geometry.block_size())
}

fn decode_group(
	bytes: &[u8; RECORD_VALUE_BYTES],
	geometry: Geometry,
) -> Result<(Fork, usize), FormatError> {
	let mut extents = [ZERO_EXTENT; EXTENTS_PER_RECORD];
	let mut blocks = 0u32;
	let mut used_extents = 0;
	for (extent, chunk) in extents.iter_mut().zip(bytes.as_chunks::<EXTENT_BYTES>().0) {
		let (start, count) = chunk
			.split_first_chunk::<{ size_of::<u32>() }>()
			.ok_or(FormatError::InvalidFork)?;
		let count = count.try_into().map_err(|_| FormatError::InvalidFork)?;
		let count = u32::from_be_bytes(count);
		*extent = Extent {
			start: AllocationBlock(u32::from_be_bytes(*start)),
			blocks: count,
		};
		blocks = blocks.checked_add(count).ok_or(FormatError::InvalidFork)?;
		used_extents += usize::from(count != 0);
	}
	if blocks == 0 {
		return Err(FormatError::InvalidFork);
	}
	let fork = Fork::new(group_bytes(blocks, geometry), 0, blocks, &extents, geometry)?;
	Ok((fork, used_extents))
}

fn push_group<S: ReadAt>(
	volume: &Volume<S>,
	groups: &mut Vec<Fork>,
	fork: Fork,
) -> Result<(), OverflowError<S::Error>> {
	volume.open_embedded_fork(&fork)?;
	groups.try_reserve(1)?;
	groups.push(fork);
	Ok(())
}

fn check_disjoint<E>(groups: &[Fork]) -> Result<(), OverflowError<E>> {
	let mut extents = Vec::new();
	let max_extents = groups
		.len()
		.checked_mul(EXTENTS_PER_RECORD)
		.ok_or(FormatError::InvalidFork)?;
	extents.try_reserve_exact(max_extents)?;
	extents.extend(groups.iter().flat_map(Fork::extents));
	extents.sort_unstable_by_key(|extent| extent.start.0);
	for pair in extents.windows(2) {
		let [previous, next] = pair else { continue };
		let previous_end = u64::from(previous.start.0) + u64::from(previous.blocks);
		if u64::from(next.start.0) < previous_end {
			return Err(FormatError::InvalidFork.into());
		}
	}
	Ok(())
}

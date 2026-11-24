use alloc::vec::Vec;

use hfsplus_core::{ByteLen, ByteOffset, ReadAt};
use hfsplus_format::{
	Error as FormatError,
	attributes::EXTENTS_PER_RECORD,
	btree::NodeId,
	volume::{AllocationBlock, Extent, Fork, Geometry},
};

use crate::{Error as VolumeError, Volume, overflow::ResolvedFork};

use super::AttributeReadError as Error;

const EMPTY_EXTENT: Extent = Extent {
	start: AllocationBlock(0),
	blocks: 0,
};

/// One validated stored attribute value. Inline bytes are retained in memory.
/// Fork-backed reads fetch only the requested logical byte range.
#[derive(Debug)]
pub struct AttributeValue<'volume, S> {
	pub(super) value: Storage<'volume, S>,
}

#[derive(Debug)]
pub(super) enum Storage<'volume, S> {
	Inline(Vec<u8>),
	Fork(ResolvedFork<'volume, S>),
}

impl<S> AttributeValue<'_, S> {
	/// Physical allocation extents in logical order, including allocated slack.
	/// Inline attributes have no separate allocation extents.
	pub fn extents(&self) -> impl Iterator<Item = Extent> + '_ {
		let fork = match &self.value {
			Storage::Inline(_) => None,
			Storage::Fork(fork) => Some(fork),
		};
		fork.into_iter().flat_map(ResolvedFork::extents)
	}
}

impl<S: ReadAt> ReadAt for AttributeValue<'_, S> {
	type Error = VolumeError<S::Error>;

	fn len(&self) -> ByteLen {
		match &self.value {
			Storage::Inline(bytes) => ByteLen::from(bytes.len()),
			Storage::Fork(fork) => fork.len(),
		}
	}

	fn read_exact_at(&self, offset: ByteOffset, destination: &mut [u8]) -> Result<(), Self::Error> {
		match &self.value {
			Storage::Inline(bytes) => bytes
				.as_slice()
				.read_exact_at(offset, destination)
				.map_err(VolumeError::Bounds),
			Storage::Fork(fork) => fork.read_exact_at(offset, destination),
		}
	}
}

pub(super) struct ForkChain {
	anchor: NodeId,
	logical_len: ByteLen,
	declared_blocks: u32,
	covered_blocks: u32,
	groups: Vec<Fork>,
}

impl ForkChain {
	pub(super) fn new<E>(node: NodeId, fork: Fork, geometry: Geometry) -> Result<Self, Error<E>> {
		let covered_blocks = fork.extents().map(|extent| extent.blocks).sum();
		let first = if fork.has_overflow() {
			let mut extents = [EMPTY_EXTENT; EXTENTS_PER_RECORD];
			for (slot, extent) in extents.iter_mut().zip(fork.extents()) {
				*slot = extent;
			}
			let bytes = u64::from(covered_blocks) * u64::from(geometry.block_size());
			Fork::new(bytes, 0, covered_blocks, &extents, geometry)
				.map_err(|error| Error::format(node, error))?
		} else {
			fork
		};
		let mut groups = Vec::new();
		groups.try_reserve_exact(1).map_err(Error::Allocation)?;
		groups.push(first);
		Ok(Self {
			anchor: node,
			logical_len: ByteLen(fork.logical_bytes),
			declared_blocks: fork.total_blocks,
			covered_blocks,
			groups,
		})
	}

	pub(super) fn append<E>(
		&mut self,
		node: NodeId,
		start_block: u32,
		group: Fork,
	) -> Result<(), Error<E>> {
		if start_block != self.covered_blocks {
			return Err(Error::InvalidStart {
				node,
				expected: self.covered_blocks,
				actual: start_block,
			});
		}
		let invalid = || Error::format(node, FormatError::InvalidAttribute);
		let covered = self
			.covered_blocks
			.checked_add(group.total_blocks)
			.filter(|&covered| covered <= self.declared_blocks)
			.ok_or_else(invalid)?;
		if covered < self.declared_blocks && group.extents().count() != EXTENTS_PER_RECORD {
			return Err(invalid());
		}
		self.groups.try_reserve(1).map_err(Error::Allocation)?;
		self.groups.push(group);
		self.covered_blocks = covered;
		Ok(())
	}

	pub(super) fn finish<S: ReadAt>(
		self,
		volume: &Volume<S>,
	) -> Result<ResolvedFork<'_, S>, Error<S::Error>> {
		if self.covered_blocks != self.declared_blocks {
			return Err(Error::IncompleteFork {
				node: self.anchor,
				covered: self.covered_blocks,
				declared: self.declared_blocks,
			});
		}
		ResolvedFork::from_extent_groups(volume, self.groups, self.logical_len).map_err(|error| {
			Error::Layout {
				node: self.anchor,
				error,
			}
		})
	}
}

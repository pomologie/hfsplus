use std::num::NonZeroU32;

use hfsplus_format::{
	extents::{EXTENTS_PER_RECORD, ForkKind, KEY_BYTES, Key, LEAF_RECORD_BYTES},
	volume::{AllocationBlock, CatalogId, Extent, Fork, Geometry},
};

use crate::{
	PlanError,
	allocation::{Allocation, AllocationError},
	tree::{PackedTree, Record, TreeOptions},
};

use super::{VolumeError, layout::RESERVED_CATALOG_FORK_WORD};

const EMPTY_EXTENT: Extent = Extent {
	start: AllocationBlock(0),
	blocks: 0,
};

/// Two logical forks that share a contiguous physical reservation.
#[derive(Clone, Copy)]
pub(super) struct InterleavedForks {
	start: AllocationBlock,
	data_blocks: u32,
	resource_blocks: u32,
	quantum: NonZeroU32,
}

impl InterleavedForks {
	pub(super) fn allocate(
		allocation: &mut Allocation,
		geometry: Geometry,
		data_bytes: u64,
		resource_bytes: u64,
		quantum: NonZeroU32,
		records: &mut OverflowRecords,
	) -> Result<Option<Self>, VolumeError> {
		let block_bytes = u64::from(geometry.block_size());
		let data_blocks = data_bytes.div_ceil(block_bytes);
		let resource_blocks = resource_bytes.div_ceil(block_bytes);
		let quantum_blocks = u64::from(quantum.get());
		if data_blocks == 0
			|| resource_blocks == 0
			|| data_blocks.max(resource_blocks) <= quantum_blocks
		{
			return Ok(None);
		}
		let requested = data_blocks + resource_blocks;
		let available = allocation.free_blocks();
		if requested > u64::from(available) {
			return Err(AllocationError::InsufficientSpace {
				requested,
				available,
			}
			.into());
		}
		let data_runs = data_blocks.div_ceil(quantum_blocks);
		let resource_runs = resource_blocks.div_ceil(quantum_blocks);
		let run_count = 2 * data_runs.min(resource_runs) + u64::from(data_runs > resource_runs);
		records.admit_runs(run_count)?;
		let reservation = allocation
			.allocate(requested * block_bytes)?
			.expect("two nonempty forks require a reservation");
		Ok(Some(Self {
			start: reservation.start,
			data_blocks: data_blocks as u32,
			resource_blocks: resource_blocks as u32,
			quantum,
		}))
	}

	/// Physical order also preserves increasing logical offset within each fork.
	pub(super) fn runs(self) -> impl Iterator<Item = (ForkKind, Extent)> {
		let mut next = self.start.0;
		let mut data = self.data_blocks;
		let mut resource = self.resource_blocks;
		let mut kind = ForkKind::Data;
		std::iter::from_fn(move || {
			if data == 0 && resource == 0 {
				return None;
			}
			let (kind_now, count) = if data != 0 && (kind == ForkKind::Data || resource == 0) {
				let count = if resource == 0 {
					data
				} else {
					data.min(self.quantum.get())
				};
				data -= count;
				kind = ForkKind::Resource;
				(ForkKind::Data, count)
			} else {
				let count = if data == 0 {
					resource
				} else {
					resource.min(self.quantum.get())
				};
				resource -= count;
				kind = ForkKind::Data;
				(ForkKind::Resource, count)
			};
			let extent = Extent {
				start: AllocationBlock(next),
				blocks: count,
			};
			next += count;
			Some((kind_now, extent))
		})
	}

	pub(super) fn describe(
		self,
		geometry: Geometry,
		id: CatalogId,
		data_bytes: u64,
		resource_bytes: u64,
		records: &mut OverflowRecords,
	) -> Result<(Fork, Fork), VolumeError> {
		let data = self.describe_fork(geometry, id, ForkKind::Data, data_bytes, records)?;
		let resource =
			self.describe_fork(geometry, id, ForkKind::Resource, resource_bytes, records)?;
		Ok((data, resource))
	}

	fn describe_fork(
		self,
		geometry: Geometry,
		id: CatalogId,
		kind: ForkKind,
		bytes: u64,
		records: &mut OverflowRecords,
	) -> Result<Fork, VolumeError> {
		let blocks = match kind {
			ForkKind::Data => self.data_blocks,
			ForkKind::Resource => self.resource_blocks,
		};
		let mut extents = self
			.runs()
			.filter_map(|(owner, extent)| (owner == kind).then_some(extent));
		let mut record = [EMPTY_EXTENT; EXTENTS_PER_RECORD];
		let mut fill = |record: &mut [Extent; EXTENTS_PER_RECORD]| {
			let mut count = 0;
			for (slot, extent) in record.iter_mut().zip(extents.by_ref()) {
				*slot = extent;
				count += 1;
			}
			count
		};
		let inline_count = fill(&mut record);
		let embedded = &record[..inline_count];
		let fork = Fork::new(
			bytes,
			RESERVED_CATALOG_FORK_WORD,
			blocks,
			embedded,
			geometry,
		)?;
		let mut start_block = embedded.iter().map(|extent| extent.blocks).sum();
		loop {
			let count = fill(&mut record);
			if count == 0 {
				break;
			}
			let extents = &record[..count];
			records.push(
				Key {
					id,
					kind,
					start_block,
				},
				extents,
			)?;
			start_block += extents.iter().map(|extent| extent.blocks).sum::<u32>();
		}
		debug_assert_eq!(start_block, blocks);
		Ok(fork)
	}
}

pub(super) struct OverflowRecords {
	records: Vec<(Key, [u8; LEAF_RECORD_BYTES])>,
	remaining_runs: usize,
	max_records: usize,
}

impl OverflowRecords {
	pub(super) fn new(max_ranges: usize, metadata_budget: usize) -> Self {
		Self {
			records: Vec::new(),
			remaining_runs: max_ranges,
			max_records: metadata_budget / LEAF_RECORD_BYTES,
		}
	}

	fn admit_runs(&mut self, count: u64) -> Result<(), PlanError> {
		let count = usize::try_from(count).map_err(|_| PlanError::RangeLimit)?;
		self.remaining_runs = self
			.remaining_runs
			.checked_sub(count)
			.ok_or(PlanError::RangeLimit)?;
		Ok(())
	}

	fn push(&mut self, key: Key, extents: &[Extent]) -> Result<(), VolumeError> {
		if self.records.len() >= self.max_records {
			return Err(VolumeError::Limit);
		}
		self.records.try_reserve(1)?;
		self.records.push((key, key.encode_record(extents)?));
		Ok(())
	}

	pub(super) fn pack(mut self, options: TreeOptions) -> Result<PackedTree, VolumeError> {
		self.records.sort_unstable_by_key(|(key, _)| *key);
		let mut leaves = Vec::new();
		leaves.try_reserve_exact(self.records.len())?;
		leaves.extend(
			self.records
				.iter()
				.map(|(_, bytes)| Record::new(bytes, KEY_BYTES)),
		);
		Ok(PackedTree::build(&leaves, options)?)
	}
}

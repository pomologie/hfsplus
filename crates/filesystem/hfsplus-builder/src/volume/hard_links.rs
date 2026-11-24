use std::num::NonZeroU32;

use hfsplus_format::{
	hard_links::{LEGACY_REFERENCE_FIRST, LEGACY_REFERENCE_LAST, LinkReference},
	volume::CatalogId,
};

use super::{
	EntryMetadata, ForkSource, HardLinkGroup, VolumeError,
	manifest::{Input, PlannedEntry},
};

#[derive(Debug)]
pub(super) struct HardLinks<S> {
	pub(super) directory: CatalogId,
	pub(super) files: Vec<LinkedFile<S>>,
}

#[derive(Debug)]
pub(super) struct LinkedFile<S> {
	pub(super) id: CatalogId,
	pub(super) reference: LinkReference,
	pub(super) link_count: NonZeroU32,
	pub(super) metadata: EntryMetadata,
	pub(super) data: ForkSource<S>,
	pub(super) resource: Option<ForkSource<S>>,
}

struct RankedGroup<'a, S> {
	index: usize,
	reference: LinkReference,
	input: HardLinkGroup<'a, S>,
}

impl<S> HardLinks<S> {
	pub(super) fn assign(
		entries: &mut [PlannedEntry<'_, S>],
		groups: Vec<HardLinkGroup<'_, S>>,
		directory: CatalogId,
	) -> Result<(Option<Self>, CatalogId), VolumeError> {
		if groups.is_empty() {
			return Ok((None, directory));
		}
		let count = u32::try_from(groups.len()).map_err(|_| VolumeError::Limit)?;
		let first_id = directory.0.checked_add(1).ok_or(VolumeError::Limit)?;
		let next_id = CatalogId(first_id.checked_add(count).ok_or(VolumeError::Limit)?);
		let mut rank = Vec::new();
		rank.try_reserve_exact(groups.len())?;
		rank.resize(groups.len(), None);
		let mut next_rank = 0usize;
		for entry in &*entries {
			if let Input::Alias { group, .. } = entry.input
				&& rank[group].is_none()
			{
				rank[group] = Some(next_rank);
				next_rank += 1;
			}
		}

		let mut ranked = Vec::new();
		ranked.try_reserve_exact(groups.len())?;
		for (index, (input, rank)) in groups.into_iter().zip(rank.iter().copied()).enumerate() {
			let rank = rank.expect("every admitted hard-link group has visible aliases");
			let reference = u32::try_from(rank)
				.ok()
				.and_then(|rank| LEGACY_REFERENCE_FIRST.checked_add(rank))
				.filter(|reference| *reference <= LEGACY_REFERENCE_LAST)
				.and_then(LinkReference::new)
				.ok_or(VolumeError::Limit)?;
			ranked.push(RankedGroup {
				index,
				reference,
				input,
			});
		}
		// The name has a fixed ASCII prefix and decimal digits. Assign IDs
		// by stored name order, including the boundary from 999 to 1000.
		ranked.sort_unstable_by(|a, b| {
			a.reference
				.inode_name()
				.units()
				.cmp(b.reference.inode_name().units())
		});

		let mut files = Vec::new();
		files.try_reserve_exact(ranked.len())?;
		for (id, group) in (first_id..next_id.0).zip(ranked) {
			let input = group.input;
			let link_count = u32::try_from(input.paths.len())
				.ok()
				.and_then(NonZeroU32::new)
				.ok_or(VolumeError::Limit)?;
			rank[group.index] = Some(files.len());
			files.push(LinkedFile {
				id: CatalogId(id),
				reference: group.reference,
				link_count,
				metadata: input.metadata,
				data: input.data,
				resource: input.resource,
			});
		}
		for entry in entries {
			if let Input::Alias { group, .. } = &mut entry.input {
				*group = rank[*group].expect("every admitted hard-link group has an inode");
			}
		}
		Ok((Some(Self { directory, files }), next_id))
	}
}

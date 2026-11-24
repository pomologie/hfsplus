use alloc::vec::Vec;

use hfsplus_core::ReadAt;
use hfsplus_format::hard_links::PRIVATE_DIRECTORY_NAME;

use super::{Catalog, CatalogProblem, CheckError, Member, Query, index_of};
use crate::catalog::{HardLinkError, HardLinkLimits, HardLinkProfile, LinkCorruption, ROOT_ID};

const CREATED_DATE_INDEX: usize = 0;

impl<S: ReadAt> Catalog<'_, S> {
	pub(super) fn check_links(
		&self,
		entries: &mut [Member],
		root: usize,
		limits: HardLinkLimits,
		query: &mut Query,
	) -> Result<u32, CheckError<S::Error>> {
		let mut aliases = Vec::new();
		for (index, member) in entries
			.iter()
			.enumerate()
			.filter(|(_, member)| member.visible())
		{
			if let Some(stub) = member.stub {
				aliases.try_reserve(1).map_err(CheckError::Memory)?;
				aliases.push((stub.reference, index, stub));
			}
		}
		aliases.sort_unstable_by_key(|&(reference, index, _)| (reference, index));
		let private_creation = entries
			.iter()
			.find(|member| {
				member.entry.parent == ROOT_ID && member.entry.name == PRIVATE_DIRECTORY_NAME
			})
			.map(|member| member.entry.metadata.dates[CREATED_DATE_INDEX]);
		let root_creation = entries[root].entry.metadata.dates[CREATED_DATE_INDEX];
		let mut groups = 0;
		for group in aliases.chunk_by(|left, right| left.0 == right.0) {
			let (reference, member, stub) = group[0];
			let selected = &entries[member].entry;
			let (inode, profile) = self.resolve_inode(selected, stub, limits, query)?;
			let actual = group.len() as u32;
			let declared = inode.link_count.get();
			if actual != declared {
				return Err(CatalogProblem::LinkCount {
					reference,
					declared,
					actual,
				}
				.into());
			}
			for &(_, index, stub) in group {
				let entry = &entries[index].entry;
				let id = entry.metadata.id;
				let creation = entry.metadata.dates[CREATED_DATE_INDEX];
				if creation != root_creation && Some(creation) != private_creation {
					return Err(HardLinkError::Corrupt {
						id,
						reason: LinkCorruption::CreationDate,
					}
					.into());
				}
				if stub.neighbors.is_some() != (profile == HardLinkProfile::VerifiedChain) {
					return Err(HardLinkError::Corrupt {
						id,
						reason: LinkCorruption::ConflictingProfiles,
					}
					.into());
				}
			}
			let id = inode.metadata.id;
			let index = index_of(entries, id).ok_or(CatalogProblem::LinkInode(id))?;
			let member = &mut entries[index];
			if member.visible() || member.referenced_inode || member.stub.is_some() {
				return Err(CatalogProblem::LinkInode(id).into());
			}
			member.referenced_inode = true;
			groups += 1;
		}
		Ok(groups)
	}
}

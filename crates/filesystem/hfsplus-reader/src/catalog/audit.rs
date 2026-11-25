use crate::alloc::vec::Vec;
use core::cmp::Ordering;

use hfsplus_core::ReadAt;
use hfsplus_format::{
	btree::NodeId,
	catalog::{FLAG_HAS_FOLDER_COUNT, Record, Value},
	hard_links::Stub,
	volume::CatalogId,
};

use super::{
	Catalog, Corruption, Entry, EntryKind, HardLinkError, ROOT_ID, copy_name, format, invalid,
	is_private,
	tree::{OwnedKey, Position, Query, node_buffer},
};
use crate::check::{CatalogProblem, CheckError, CheckLimit, CheckLimits, FolderCounts};

mod links;

pub(crate) struct Audit {
	pub(crate) entries: Vec<Member>,
	pub(crate) files: u32,
	pub(crate) folders: u32,
	pub(crate) link_groups: u32,
	pub(crate) unexamined_private_files: u32,
	pub(crate) unsupported_directory_links: u32,
}

pub(crate) struct Member {
	pub(crate) entry: Entry,
	stub: Option<Stub>,
	state: State,
	parent: Option<usize>,
	children: u32,
	child_folders: u32,
	referenced_inode: bool,
}

#[derive(Clone, Copy)]
enum State {
	Unseen,
	Walking,
	Rooted { depth: usize, private: bool },
}

impl Member {
	pub(crate) fn owns_reachable_storage(&self) -> bool {
		self.referenced_inode || (self.visible() && self.stub.is_none())
	}

	fn visible(&self) -> bool {
		matches!(self.state, State::Rooted { private: false, .. })
	}
}

impl<S: ReadAt> Catalog<'_, S> {
	pub(crate) fn audit(
		&self,
		limits: CheckLimits,
		folder_counts: FolderCounts,
	) -> Result<Audit, CheckError<S::Error>> {
		let mut query = self.query()?;
		self.tree.audit_index_graph(&mut query)?;
		let mut entries = self.collect_records(limits.max_catalog_entries, &mut query)?;
		let root = validate_hierarchy(&mut entries, limits.catalog.max_ancestors, folder_counts)?;
		let (files, folders) = self.check_catalog_counts(&entries)?;
		let link_groups = self.check_links(&mut entries, root, limits.hard_links, &mut query)?;
		let unexamined_private_files = entries
			.iter()
			.filter(|member| {
				!member.visible()
					&& !member.referenced_inode
					&& matches!(member.entry.kind, EntryKind::File { .. })
			})
			.count() as u32;
		let unsupported_directory_links = entries
			.iter()
			.filter(|member| super::hard_links::is_directory_link(&member.entry))
			.count() as u32;
		Ok(Audit {
			entries,
			files,
			folders,
			link_groups,
			unexamined_private_files,
			unsupported_directory_links,
		})
	}

	fn collect_records(
		&self,
		limit: usize,
		query: &mut Query,
	) -> Result<Vec<Member>, CheckError<S::Error>> {
		let tree = &self.tree;
		let header = tree.header();
		let mut id = header.first_leaf.ok_or(CatalogProblem::MissingRoot)?;
		let mut entries = Vec::new();
		let mut scan = node_buffer(tree.node_bytes())?;
		let mut predecessor: Option<(NodeId, OwnedKey)> = None;
		let mut threads = 0;
		let mut actual = 0u32;
		loop {
			let leaf = tree.read_leaf(id, &mut scan, query)?;
			if let Some((prev, key)) = predecessor.take() {
				let first = leaf.record(0).ok_or(invalid(id, Corruption::RecordOrder))?;
				let (first, _) = tree.split_key(id, first)?;
				if leaf.backward() != Some(prev)
					|| key.compare(first, tree.collation()) != Ordering::Less
				{
					return Err(invalid(id, Corruption::NodeLinks).into());
				}
			}
			for (ordinal, raw) in leaf.records().enumerate() {
				actual = actual.checked_add(1).ok_or(CatalogProblem::LeafRecords {
					declared: header.leaf_records,
					actual,
				})?;
				if actual > header.leaf_records {
					return Err(CatalogProblem::LeafRecords {
						declared: header.leaf_records,
						actual,
					}
					.into());
				}
				let (key, _) = tree.split_key(id, raw)?;
				let name = copy_name(key.name)?;
				let position = Position { node: id, ordinal };
				if tree.find(key.parent_or_thread, &name, query)? != Some(position) {
					return Err(invalid(id, Corruption::IndexRoute).into());
				}
				if key.name.is_empty() {
					self.decode_thread(position, raw)?;
					threads += 1;
					continue;
				}
				if entries.len() >= limit {
					return Err(CheckError::Limit(CheckLimit::CatalogEntries));
				}
				let entry = self.decode_entry(position, raw)?;
				self.check_thread(&entry, query)?;
				let record =
					Record::decode(raw, self.geometry).map_err(|error| format(id, error))?;
				let stub = if matches!(record.value, Value::File { .. }) {
					Stub::decode(&record).map_err(|error| HardLinkError::Codec {
						id: entry.metadata.id,
						error,
					})?
				} else {
					None
				};
				entries.try_reserve(1).map_err(CheckError::Memory)?;
				entries.push(Member {
					entry,
					stub,
					state: State::Unseen,
					parent: None,
					children: 0,
					child_folders: 0,
					referenced_inode: false,
				});
			}
			let Some(next) = leaf.forward() else {
				break;
			};
			let raw = leaf
				.records()
				.last()
				.ok_or(invalid(id, Corruption::RecordOrder))?;
			let (last, _) = tree.split_key(id, raw)?;
			predecessor = Some((id, OwnedKey::from_key(last)));
			id = next;
		}
		if actual != header.leaf_records {
			return Err(CatalogProblem::LeafRecords {
				declared: header.leaf_records,
				actual,
			}
			.into());
		}
		let records = entries.len() as u32;
		if threads != records {
			return Err(CatalogProblem::ThreadCount { records, threads }.into());
		}
		Ok(entries)
	}

	fn check_catalog_counts(&self, entries: &[Member]) -> Result<(u32, u32), CheckError<S::Error>> {
		let files = entries
			.iter()
			.filter(|member| matches!(member.entry.kind, EntryKind::File { .. }))
			.count() as u32;
		let folders = entries.len() as u32 - files - 1;
		let header = self.volume.header();
		if header.files != files {
			return Err(CatalogProblem::FileCount {
				declared: header.files,
				actual: files,
			}
			.into());
		}
		if header.folders != folders {
			return Err(CatalogProblem::FolderCount {
				declared: header.folders,
				actual: folders,
			}
			.into());
		}
		Ok((files, folders))
	}
}

fn validate_hierarchy<E>(
	entries: &mut [Member],
	max_depth: usize,
	folder_counts: FolderCounts,
) -> Result<usize, CheckError<E>> {
	entries.sort_unstable_by_key(|member| member.entry.metadata.id);
	for pair in entries.windows(2) {
		if pair[0].entry.metadata.id == pair[1].entry.metadata.id {
			return Err(CatalogProblem::DuplicateId(pair[0].entry.metadata.id).into());
		}
	}
	let root = index_of(entries, ROOT_ID).ok_or(CatalogProblem::MissingRoot)?;
	entries[root].state = State::Rooted {
		depth: 0,
		private: false,
	};
	for index in 0..entries.len() {
		if index == root {
			continue;
		}
		let child = entries[index].entry.metadata.id;
		let parent = entries[index].entry.parent;
		let parent_index = index_of(entries, parent)
			.filter(|&index| matches!(entries[index].entry.kind, EntryKind::Directory { .. }))
			.ok_or(CatalogProblem::Parent { child, parent })?;
		entries[index].parent = Some(parent_index);
		entries[parent_index].children += 1;
		entries[parent_index].child_folders += u32::from(matches!(
			entries[index].entry.kind,
			EntryKind::Directory { .. }
		));
	}
	for member in entries.iter() {
		let EntryKind::Directory { valence, tail_word } = member.entry.kind else {
			continue;
		};
		let id = member.entry.metadata.id;
		if valence != member.children {
			return Err(CatalogProblem::Valence {
				id,
				declared: valence,
				actual: member.children,
			}
			.into());
		}
		if folder_counts == FolderCounts::Require {
			if member.entry.metadata.flags & FLAG_HAS_FOLDER_COUNT == 0 {
				return Err(CatalogProblem::MissingFolderCount(id).into());
			}
			if tail_word != member.child_folders {
				return Err(CatalogProblem::SubfolderCount {
					id,
					declared: tail_word,
					actual: member.child_folders,
				}
				.into());
			}
		}
	}

	let mut stack = Vec::new();
	for index in 0..entries.len() {
		let mut current = index;
		let (mut depth, mut private) = loop {
			let entry = &mut entries[current];
			match entry.state {
				State::Rooted { depth, private } => break (depth, private),
				State::Walking => {
					return Err(CatalogProblem::ParentCycle(entry.entry.metadata.id).into());
				}
				State::Unseen => {
					if stack.len() >= max_depth {
						return Err(CheckError::Limit(CheckLimit::ParentDepth));
					}
					stack.try_reserve(1).map_err(CheckError::Memory)?;
					stack.push(current);
					entry.state = State::Walking;
					current = entry.parent.ok_or(CatalogProblem::MissingRoot)?;
				}
			}
		};
		while let Some(index) = stack.pop() {
			depth += 1;
			if depth > max_depth {
				return Err(CheckError::Limit(CheckLimit::ParentDepth));
			}
			private |= is_private(&entries[index].entry);
			entries[index].state = State::Rooted { depth, private };
		}
	}
	Ok(root)
}

fn index_of(entries: &[Member], id: CatalogId) -> Option<usize> {
	entries
		.binary_search_by_key(&id, |member| member.entry.metadata.id)
		.ok()
}

#[cfg(test)]
mod tests;

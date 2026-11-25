use crate::alloc::vec::Vec;
use core::{cmp::Ordering, num::NonZeroUsize};

use hfsplus_core::ReadAt;
use hfsplus_format::{btree::NodeId, volume::CatalogId};

use super::{
	Catalog, CatalogError, Corruption, Entry, EntryKind, Limit, invalid,
	tree::{OwnedKey, Position, Query, node_buffer},
};

/// Borrowed cursor over one directory in the catalog's immutable source.
#[derive(Debug)]
pub struct Directory<'catalog, 'volume, S> {
	catalog: &'catalog Catalog<'volume, S>,
	parent: CatalogId,
	valence: u32,
	state: State,
}

#[derive(Debug, Clone, Copy)]
struct State {
	position: Option<Position>,
	seen: u32,
}

/// One bounded page of visible directory entries.
#[derive(Debug)]
#[non_exhaustive]
pub struct DirectoryPage {
	/// Entries in the catalog's native key order.
	pub entries: Vec<Entry>,
	/// The scan reached the directory end and validated its stored child count.
	pub complete: bool,
}

impl<'volume, S: ReadAt> Catalog<'volume, S> {
	/// Opens a directory scan after validating the directory and its thread.
	/// The cursor follows the bounded leaf chain lazily.
	/// # Errors
	/// Rejects an inaccessible parent or catalog corruption and budget failures.
	pub fn read_dir(
		&self,
		parent: CatalogId,
	) -> Result<Directory<'_, 'volume, S>, CatalogError<S::Error>> {
		let mut query = self.query()?;
		let EntryKind::Directory { valence, .. } = self.directory_entry(parent, &mut query)?.kind
		else {
			return Err(CatalogError::ExpectedDirectory(parent));
		};
		let position = self
			.tree
			.find(parent, &[], &mut query)?
			.ok_or(invalid(super::tree::HEADER_ID, Corruption::Thread))?;
		Ok(Directory {
			catalog: self,
			parent,
			valence,
			state: State {
				position: Some(Position {
					ordinal: position.ordinal + 1,
					..position
				}),
				seen: 0,
			},
		})
	}
}

impl<S: ReadAt> Directory<'_, '_, S> {
	/// Reads up to `size` entries. A successful page with `complete == false`
	/// may need one more call to verify the directory end without entries.
	/// Internal catalog entries count toward valence and are omitted.
	/// # Errors
	/// Rejects requests above the configured page limit, malformed links,
	/// incomplete or inconsistent entries, and source or resource failures.
	/// A failed page leaves the cursor at the prior successful position.
	pub fn next_page(
		&mut self,
		size: NonZeroUsize,
	) -> Result<DirectoryPage, CatalogError<S::Error>> {
		let size = size.get();
		if size > self.catalog.limits.max_page_entries {
			return Err(CatalogError::Limit(Limit::PageEntries));
		}
		let mut entries = Vec::new();
		let mut state = self.state;
		if state.position.is_some() {
			let mut query = self.catalog.query()?;
			let mut scan = node_buffer(self.catalog.tree.node_bytes())?;
			self.read_page(size, &mut state, &mut entries, &mut query, &mut scan)?;
		}
		self.state = state;
		Ok(DirectoryPage {
			entries,
			complete: state.position.is_none(),
		})
	}

	fn read_page(
		&self,
		size: usize,
		state: &mut State,
		entries: &mut Vec<Entry>,
		query: &mut Query,
		scan: &mut [u8],
	) -> Result<(), CatalogError<S::Error>> {
		let tree = &self.catalog.tree;
		let mut predecessor: Option<(NodeId, OwnedKey)> = None;
		while let Some(position) = state.position {
			let id = position.node;
			let node = tree.read_leaf(id, scan, query)?;
			if let Some((previous_node, previous_key)) = predecessor.take() {
				let raw = node.record(0).ok_or(invalid(id, Corruption::RecordOrder))?;
				let (first, _) = tree.split_key(id, raw)?;
				if node.backward() != Some(previous_node)
					|| previous_key.compare(first, tree.collation()) != Ordering::Less
				{
					return Err(invalid(id, Corruption::NodeLinks));
				}
			}
			for (ordinal, raw) in node.records().enumerate().skip(position.ordinal) {
				let (key, _) = tree.split_key(id, raw)?;
				match key.parent_or_thread.cmp(&self.parent) {
					Ordering::Less => return Err(invalid(id, Corruption::RecordOrder)),
					Ordering::Greater => return self.finish(id, state),
					Ordering::Equal => {}
				}
				let position = Position { node: id, ordinal };
				if let Some(entry) = self.consume(position, raw, state, query)? {
					entries.try_reserve(1).map_err(CatalogError::Allocation)?;
					entries.push(entry);
					if entries.len() == size {
						return Ok(());
					}
				}
			}
			let Some(next) = node.forward() else {
				return self.finish(id, state);
			};
			let last = node
				.records()
				.last()
				.ok_or(invalid(id, Corruption::RecordOrder))?;
			let (last, _) = tree.split_key(id, last)?;
			predecessor = Some((id, OwnedKey::from_key(last)));
			state.position = Some(Position {
				node: next,
				ordinal: 0,
			});
		}
		Ok(())
	}

	fn consume(
		&self,
		position: Position,
		raw: &[u8],
		state: &mut State,
		query: &mut Query,
	) -> Result<Option<Entry>, CatalogError<S::Error>> {
		let entry = self.catalog.decode_entry(position, raw)?;
		self.catalog.check_thread(&entry, query)?;
		if self.catalog.tree.find(entry.parent, &entry.name, query)? != Some(position) {
			return Err(invalid(position.node, Corruption::IndexRoute));
		}
		state.seen = state
			.seen
			.checked_add(1)
			.filter(|&seen| seen <= self.valence)
			.ok_or(invalid(position.node, Corruption::Valence))?;
		state.position = Some(Position {
			ordinal: position.ordinal + 1,
			..position
		});
		Ok((!self.catalog.is_internal(&entry)).then_some(entry))
	}

	fn finish(&self, node: NodeId, state: &mut State) -> Result<(), CatalogError<S::Error>> {
		if state.seen != self.valence {
			return Err(invalid(node, Corruption::Valence));
		}
		state.position = None;
		Ok(())
	}
}

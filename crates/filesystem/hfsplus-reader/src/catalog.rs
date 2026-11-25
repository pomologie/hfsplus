//! Bounded catalog lookup and paged directory reads for clean HFS-family volumes.

use crate::alloc::vec::Vec;

use hfsplus_core::{
	ReadAt,
	names::{MAX_NAME_UNITS, NameCollation, normalize},
};
use hfsplus_format::{
	btree::NodeId,
	catalog::{RawName, Record, ThreadKind, Value},
	hard_links::PRIVATE_DIRECTORY_NAME,
	volume::{CNID_REUSE_ATTRIBUTE, CatalogId, Fork, Geometry},
};

use crate::{ForkReader, SpecialFile, Volume};

mod audit;
mod directory;
mod error;
mod hard_links;
mod tree;

pub use directory::{Directory, DirectoryPage};
pub use error::{CatalogError, Corruption, Limit};
pub use hard_links::{
	HardLink, HardLinkError, HardLinkLimits, HardLinkProfile, LinkCorruption, LinkTarget,
	RegularFile,
};

use tree::{Position, Query, Tree};

/// CNID of the volume's root directory.
pub const ROOT_ID: CatalogId = CatalogId(2);
const ROOT_PARENT_ID: CatalogId = CatalogId(1);
const DEFAULT_NODES_PER_QUERY: u32 = 2048;
const DEFAULT_MAP_BYTES: usize = 64 * 1024;
const DEFAULT_DEPTH: u8 = 16;
const DEFAULT_ANCESTORS: usize = 64;
const DEFAULT_PAGE_ENTRIES: usize = 128;
const PRIVATE_DIRECTORY_LINK_DIRECTORY: &str = ".HFS+ Private Directory Data\r";
const JOURNAL_FILE: &str = ".journal";
const JOURNAL_INFO_FILE: &str = ".journal_info_block";

/// Bound the work and allocations associated with an open catalog.
#[derive(Debug, Clone, Copy)]
pub struct CatalogLimits {
	/// Maximum visited B-tree nodes for one lookup or directory page.
	pub max_nodes_per_query: u32,
	/// Maximum cached allocation bitmap bytes.
	pub max_map_bytes: usize,
	/// Maximum B-tree height.
	pub max_depth: u8,
	/// Maximum parent hops to the root in a query.
	pub max_ancestors: usize,
	/// Maximum number of entries returned on one directory page.
	pub max_page_entries: usize,
}

impl Default for CatalogLimits {
	fn default() -> Self {
		Self {
			max_nodes_per_query: DEFAULT_NODES_PER_QUERY,
			max_map_bytes: DEFAULT_MAP_BYTES,
			max_depth: DEFAULT_DEPTH,
			max_ancestors: DEFAULT_ANCESTORS,
			max_page_entries: DEFAULT_PAGE_ENTRIES,
		}
	}
}

/// Raw file metadata and validated links between its catalog record and thread.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Entry {
	/// Parent from the forward record and matching thread.
	pub parent: CatalogId,
	/// Stored UTF-16 name, including any ill-formed units.
	pub name: Vec<u16>,
	/// File or folder metadata as stored.
	pub metadata: hfsplus_format::catalog::Metadata,
	/// Record kind and additional stored fields.
	pub kind: EntryKind,
}

/// Stored catalog entry kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EntryKind {
	/// Directory with a stored count of immediate items.
	Directory {
		/// Number of immediate file and folder records.
		valence: u32,
		/// Raw final word, which some profiles use as a folder count.
		tail_word: u32,
	},
	/// File record carrying raw fork descriptors. Link and compression
	/// semantics are determined separately from the catalog layout.
	File { data: Fork, resource: Fork },
}

impl EntryKind {
	const fn thread_kind(self) -> ThreadKind {
		match self {
			Self::Directory { .. } => ThreadKind::Folder,
			Self::File { .. } => ThreadKind::File,
		}
	}
}

/// Immutable view of a catalog tied to the bounded volume's source.
#[derive(Debug)]
pub struct Catalog<'volume, S> {
	tree: Tree<ForkReader<'volume, S>>,
	volume: &'volume Volume<S>,
	geometry: Geometry,
	next_id: CatalogId,
	reused_ids: bool,
	limits: CatalogLimits,
}

impl<S: ReadAt> Volume<S> {
	/// Opens the catalog through its embedded fork and verifies its B-tree
	/// header and allocation map. Lookups validate the nodes they visit.
	/// # Errors
	/// Rejects unsupported volume or catalog profiles, malformed layout,
	/// failed reads and resource limits.
	pub fn open_catalog(
		&self,
		limits: CatalogLimits,
	) -> Result<Catalog<'_, S>, CatalogError<S::Error>> {
		let header = self.header();
		let fork = self
			.open_special_fork(SpecialFile::Catalog)
			.map_err(CatalogError::Read)?;
		Ok(Catalog {
			tree: Tree::open(fork, header.signature, limits)?,
			volume: self,
			geometry: header.geometry,
			next_id: header.next_catalog_id,
			reused_ids: header.attributes & CNID_REUSE_ATTRIBUTE != 0,
			limits,
		})
	}
}

impl<S: ReadAt> Catalog<'_, S> {
	/// Returns whether this catalog was opened from that exact volume view.
	#[must_use]
	pub fn belongs_to(&self, volume: &Volume<S>) -> bool {
		core::ptr::eq(self.volume, volume)
	}

	/// The catalog's validated name comparison mode.
	#[must_use]
	pub const fn collation(&self) -> NameCollation {
		self.tree.collation()
	}

	/// Resolves the root catalog directory and validates its forward record.
	/// # Errors
	/// Fails for a missing or inconsistent root or an invalid B-tree path.
	pub fn root(&self) -> Result<Entry, CatalogError<S::Error>> {
		self.entry(ROOT_ID)?
			.ok_or(invalid(tree::HEADER_ID, Corruption::MissingRoot))
	}

	/// Resolves a catalog ID through its thread and checks its ancestry.
	/// Returns `None` if the ID cannot be found or names private metadata.
	/// # Errors
	/// Returns an error for malformed routing, broken threads, missing or
	/// cyclic ancestors, unsupported records, or exhausted budgets.
	pub fn entry(&self, id: CatalogId) -> Result<Option<Entry>, CatalogError<S::Error>> {
		let mut query = self.query()?;
		self.resolve_public(id, &mut query)
	}

	/// Finds one child name after historical HFS+ decomposition.
	/// # Errors
	/// Returns an error if the parent is unavailable or the catalog, name, or
	/// visited metadata cannot be read within the admitted profile.
	pub fn lookup(
		&self,
		parent: CatalogId,
		name: &str,
	) -> Result<Option<Entry>, CatalogError<S::Error>> {
		let name = normalize(name).map_err(CatalogError::Name)?;
		self.lookup_raw(parent, name.as_units())
	}

	/// Finds a child using stored UTF-16 units and the catalog's comparison mode.
	/// The raw input must already be decomposed where applicable.
	/// # Errors
	/// Rejects empty or overlong child names and invalid parent or catalog data.
	pub fn lookup_raw(
		&self,
		parent: CatalogId,
		name: &[u16],
	) -> Result<Option<Entry>, CatalogError<S::Error>> {
		if name.len() > MAX_NAME_UNITS || self.tree.collates_empty(name) {
			return Err(CatalogError::InvalidName);
		}
		let mut query = self.query()?;
		self.directory_entry(parent, &mut query)?;
		let Some(position) = self.tree.find(parent, name, &mut query)? else {
			return Ok(None);
		};
		let entry = self.decode_entry(position, query.record(position)?)?;
		self.check_thread(&entry, &mut query)?;
		Ok((!self.is_internal(&entry)).then_some(entry))
	}

	fn query(&self) -> Result<Query, CatalogError<S::Error>> {
		Query::new(self.tree.node_bytes(), self.limits.max_nodes_per_query)
	}

	fn directory_entry(
		&self,
		id: CatalogId,
		query: &mut Query,
	) -> Result<Entry, CatalogError<S::Error>> {
		match self.resolve_public(id, query)? {
			Some(entry) if matches!(entry.kind, EntryKind::Directory { .. }) => Ok(entry),
			_ => Err(CatalogError::ExpectedDirectory(id)),
		}
	}

	fn resolve_public(
		&self,
		id: CatalogId,
		query: &mut Query,
	) -> Result<Option<Entry>, CatalogError<S::Error>> {
		if !self.valid_id(id) {
			return Ok(None);
		}
		let Some(entry) = self.resolve(id, query)? else {
			return Ok(None);
		};
		if self.is_internal(&entry) {
			return Ok(None);
		}
		let mut seen = Vec::new();
		let mut child = id;
		let mut parent = entry.parent;
		while child != ROOT_ID {
			if seen.len() >= self.limits.max_ancestors {
				return Err(CatalogError::Limit(Limit::Ancestors));
			}
			seen.try_reserve(1).map_err(CatalogError::Allocation)?;
			seen.push(child);
			if seen.contains(&parent) {
				return Err(invalid(tree::HEADER_ID, Corruption::ParentCycle));
			}
			let ancestor = self
				.resolve(parent, query)?
				.ok_or(invalid(tree::HEADER_ID, Corruption::Parent))?;
			if !matches!(ancestor.kind, EntryKind::Directory { .. }) {
				return Err(invalid(tree::HEADER_ID, Corruption::Parent));
			}
			if self.is_internal(&ancestor) {
				return Ok(None);
			}
			child = ancestor.metadata.id;
			parent = ancestor.parent;
		}
		Ok(Some(entry))
	}

	fn resolve(
		&self,
		id: CatalogId,
		query: &mut Query,
	) -> Result<Option<Entry>, CatalogError<S::Error>> {
		let Some(position) = self.tree.find(id, &[], query)? else {
			return Ok(None);
		};
		let thread = self.decode_thread(position, query.record(position)?)?;
		let name = copy_name(thread.name)?;
		let Thread { kind, parent, .. } = thread;
		let position = self
			.tree
			.find(parent, &name, query)?
			.ok_or(invalid(position.node, Corruption::Thread))?;
		let entry = self.decode_entry(position, query.record(position)?)?;
		if entry.metadata.id != id || entry.name != name || entry.kind.thread_kind() != kind {
			return Err(invalid(position.node, Corruption::Thread));
		}
		Ok(Some(entry))
	}

	fn check_thread(&self, entry: &Entry, query: &mut Query) -> Result<(), CatalogError<S::Error>> {
		let position = self
			.tree
			.find(entry.metadata.id, &[], query)?
			.ok_or(invalid(tree::HEADER_ID, Corruption::Thread))?;
		let thread = self.decode_thread(position, query.record(position)?)?;
		if thread.parent != entry.parent
			|| thread.kind != entry.kind.thread_kind()
			|| !thread.name.units().eq(entry.name.iter().copied())
		{
			return Err(invalid(position.node, Corruption::Thread));
		}
		Ok(())
	}

	fn decode_entry(
		&self,
		position: Position,
		bytes: &[u8],
	) -> Result<Entry, CatalogError<S::Error>> {
		let record =
			Record::decode(bytes, self.geometry).map_err(|error| format(position.node, error))?;
		let (metadata, kind) = match record.value {
			Value::Folder {
				metadata,
				valence,
				tail_word,
			} => (metadata, EntryKind::Directory { valence, tail_word }),
			Value::File {
				metadata,
				data,
				resource,
			} => (metadata, EntryKind::File { data, resource }),
			Value::Thread { .. } => return Err(invalid(position.node, Corruption::Thread)),
		};
		let parent = record.key.parent_or_thread;
		let valid_parent = if metadata.id == ROOT_ID {
			parent == ROOT_PARENT_ID && matches!(kind, EntryKind::Directory { .. })
		} else {
			self.valid_id(parent) && parent != metadata.id
		};
		if !self.valid_id(metadata.id) || !valid_parent {
			return Err(invalid(position.node, Corruption::Parent));
		}
		Ok(Entry {
			parent,
			name: copy_name(record.key.name)?,
			metadata,
			kind,
		})
	}

	fn decode_thread<'bytes>(
		&self,
		position: Position,
		bytes: &'bytes [u8],
	) -> Result<Thread<'bytes>, CatalogError<S::Error>> {
		let record =
			Record::decode(bytes, self.geometry).map_err(|error| format(position.node, error))?;
		let Value::Thread { kind, parent, name } = record.value else {
			return Err(invalid(position.node, Corruption::Thread));
		};
		let id = record.key.parent_or_thread;
		let valid_parent = if id == ROOT_ID {
			parent == ROOT_PARENT_ID && kind == ThreadKind::Folder
		} else {
			self.valid_id(parent) && parent != id
		};
		if !self.valid_id(id) || !valid_parent {
			return Err(invalid(position.node, Corruption::Thread));
		}
		Ok(Thread { kind, parent, name })
	}

	fn valid_id(&self, id: CatalogId) -> bool {
		id == ROOT_ID || (id >= CatalogId::FIRST_USER && (self.reused_ids || id < self.next_id))
	}

	fn is_internal(&self, entry: &Entry) -> bool {
		is_private(entry)
			|| (self.volume.header().journal_info.is_some()
				&& entry.parent == ROOT_ID
				&& matches!(entry.kind, EntryKind::File { .. })
				&& [JOURNAL_FILE, JOURNAL_INFO_FILE]
					.into_iter()
					.any(|name| name.encode_utf16().eq(entry.name.iter().copied())))
	}
}

struct Thread<'bytes> {
	kind: ThreadKind,
	parent: CatalogId,
	name: RawName<'bytes>,
}

fn copy_name<E>(name: RawName<'_>) -> Result<Vec<u16>, CatalogError<E>> {
	let mut units = Vec::new();
	units
		.try_reserve_exact(name.len())
		.map_err(CatalogError::Allocation)?;
	units.extend(name.units());
	Ok(units)
}

fn is_private(entry: &Entry) -> bool {
	entry.parent == ROOT_ID
		&& (entry.name == PRIVATE_DIRECTORY_NAME
			|| PRIVATE_DIRECTORY_LINK_DIRECTORY
				.encode_utf16()
				.eq(entry.name.iter().copied()))
}

const fn invalid<E>(node: NodeId, reason: Corruption) -> CatalogError<E> {
	CatalogError::Corrupt { node, reason }
}

const fn format<E>(node: NodeId, error: hfsplus_format::Error) -> CatalogError<E> {
	CatalogError::Format { node, error }
}

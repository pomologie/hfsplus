//! Resolve visible regular-file records to the catalog identities that own storage.

use alloc::vec::Vec;
use core::num::NonZeroU32;

use hfsplus_core::ReadAt;
use hfsplus_format::{
	catalog::{Metadata, Record},
	hard_links::{Inode, LinkReference, PRIVATE_DIRECTORY_NAME, Stub},
	volume::{CatalogId, Fork},
};

use super::{Catalog, Entry, EntryKind, ROOT_ID, tree::Query};
use crate::{
	Volume,
	overflow::{ForkKind, OverflowError, OverflowLimits, ResolvedFork},
};

mod error;

pub use error::{HardLinkError, LinkCorruption, LinkTarget};

const DEFAULT_CHAIN_LINKS: u32 = 512;
const CREATED_DATE_INDEX: usize = 0;
const BSD_TYPE_MASK: u16 = 0o170000;
const BSD_REGULAR_FILE: u16 = 0o100000;
const DIRECTORY_LINK_FINDER_CODE: &[u8] = b"fdrpMACS";

/// Bounded work for following an inode's linked-stub chain.
#[derive(Debug, Clone, Copy)]
pub struct HardLinkLimits {
	/// Maximum stub count verified in a chain. The catalog's query budget also
	/// applies to all reads during resolution.
	pub max_chain_links: u32,
}

impl Default for HardLinkLimits {
	fn default() -> Self {
		Self {
			max_chain_links: DEFAULT_CHAIN_LINKS,
		}
	}
}

/// Evidence associated with an effective hard-link count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HardLinkProfile {
	/// The stored reference resolved to an exact, valid private inode. Its
	/// declared count has no chain to check during lookup.
	ChainFree,
	/// The inode's complete chain matched its declared count and included the
	/// selected entry with consistent reciprocal pointers.
	VerifiedChain,
}

/// The effective hard-link count, storage reference, and available evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct HardLink {
	pub reference: LinkReference,
	pub count: NonZeroU32,
	pub profile: HardLinkProfile,
}

/// A regular file whose visible identity and storage identity are both known.
/// The volume borrow ensures fork reads use the catalog's selected source.
#[derive(Debug)]
pub struct RegularFile<'volume, S> {
	volume: &'volume Volume<S>,
	id: CatalogId,
	parent: CatalogId,
	name: Vec<u16>,
	metadata: Metadata,
	data: Fork,
	resource: Fork,
	hard_link: Option<HardLink>,
}

impl<S> RegularFile<'_, S> {
	/// ID of the visible catalog entry used for this lookup.
	#[must_use]
	pub const fn id(&self) -> CatalogId {
		self.id
	}

	/// Parent of the visible catalog entry.
	#[must_use]
	pub const fn parent(&self) -> CatalogId {
		self.parent
	}

	/// Exact stored UTF-16 name of the visible entry.
	#[must_use]
	pub fn name(&self) -> &[u16] {
		&self.name
	}

	/// Catalog identity owning this file's stored forks and attributes.
	#[must_use]
	pub const fn inode_id(&self) -> CatalogId {
		self.metadata.id
	}

	/// Effective inode metadata, including ownership, mode, and timestamps.
	#[must_use]
	pub const fn metadata(&self) -> &Metadata {
		&self.metadata
	}

	/// Link reference and count evidence, if the file is a hard link.
	#[must_use]
	pub const fn hard_link(&self) -> Option<HardLink> {
		self.hard_link
	}
}

impl<S: ReadAt> RegularFile<'_, S> {
	/// Opens the chosen stored fork using its real owner CNID for overflow
	/// records. Compressed-file interpretation is separate from this API.
	/// # Errors
	/// Returns an error when the stored fork is corrupt or exceeds its read
	/// limits, or the selected extents layout is unavailable.
	pub fn open_stored_fork(
		&self,
		kind: ForkKind,
		limits: OverflowLimits,
	) -> Result<ResolvedFork<'_, S>, OverflowError<S::Error>> {
		let fork = match kind {
			ForkKind::Data => &self.data,
			ForkKind::Resource => &self.resource,
		};
		self.volume.open_fork(self.inode_id(), kind, fork, limits)
	}
}

impl<'volume, S: ReadAt> Catalog<'volume, S> {
	/// Resolves a visible regular file, following an indirect file record
	/// through its exact private inode key when necessary. Returns `None`
	/// for missing or inaccessible private catalog IDs.
	/// # Errors
	/// Rejects directories, unsupported link kinds, dangling references,
	/// corrupt chains, failed reads and resource limits.
	pub fn regular_file(
		&self,
		id: CatalogId,
		limits: HardLinkLimits,
	) -> Result<Option<RegularFile<'volume, S>>, HardLinkError<S::Error>> {
		let mut query = self.query()?;
		let Some(entry) = self.resolve_public(id, &mut query)? else {
			return Ok(None);
		};
		if is_directory_link(&entry) {
			return Err(HardLinkError::DirectoryLink(id));
		}
		let EntryKind::File { data, resource } = entry.kind else {
			return Err(HardLinkError::NotRegular(id));
		};
		let stub = self.decode_stub(&entry, &mut query)?;
		let (metadata, data, resource, hard_link) = if let Some(stub) = stub {
			let (inode, profile) = self.resolve_inode(&entry, stub, limits, &mut query)?;
			let link = HardLink {
				reference: inode.reference,
				count: inode.link_count,
				profile,
			};
			(inode.metadata, inode.data, inode.resource, Some(link))
		} else {
			if entry.metadata.permissions.mode & BSD_TYPE_MASK != BSD_REGULAR_FILE {
				return Err(HardLinkError::NotRegular(id));
			}
			(entry.metadata, data, resource, None)
		};
		Ok(Some(RegularFile {
			volume: self.volume,
			id,
			parent: entry.parent,
			name: entry.name,
			metadata,
			data,
			resource,
			hard_link,
		}))
	}

	pub(super) fn resolve_inode(
		&self,
		entry: &Entry,
		stub: Stub,
		limits: HardLinkLimits,
		query: &mut Query,
	) -> Result<(Inode, HardLinkProfile), HardLinkError<S::Error>> {
		let id = entry.metadata.id;
		let directory = self.private_directory(id, query)?;
		let root = self
			.resolve(ROOT_ID, query)?
			.ok_or(dangling(id, LinkTarget::Root))?;
		let created = [
			directory.metadata.dates[CREATED_DATE_INDEX],
			root.metadata.dates[CREATED_DATE_INDEX],
		];
		if !created.contains(&entry.metadata.dates[CREATED_DATE_INDEX]) {
			return Err(corrupt(id, LinkCorruption::CreationDate));
		}

		let name = stub.reference.inode_name();
		let position = self
			.tree
			.find(directory.metadata.id, name.units(), query)?
			.ok_or(dangling(id, LinkTarget::Inode(stub.reference)))?;
		let raw = query.record(position)?;
		let inode = Inode::decode(raw, directory.metadata.id, self.geometry)
			.map_err(|error| HardLinkError::Codec { id, error })?
			.filter(|inode| inode.reference == stub.reference)
			.ok_or(corrupt(id, LinkCorruption::InodeKey))?;
		let private_entry = self.decode_entry(position, raw)?;
		self.check_thread(&private_entry, query)?;

		let profile = match (inode.first_link, stub.neighbors) {
			(None, None) => HardLinkProfile::ChainFree,
			(Some(head), Some(_)) => {
				if inode.link_count.get() > limits.max_chain_links {
					return Err(HardLinkError::ChainLimit(id));
				}
				self.verify_chain(entry, inode, head, created, query)?;
				HardLinkProfile::VerifiedChain
			}
			_ => return Err(corrupt(id, LinkCorruption::ConflictingProfiles)),
		};
		Ok((inode, profile))
	}

	fn private_directory(
		&self,
		id: CatalogId,
		query: &mut Query,
	) -> Result<Entry, HardLinkError<S::Error>> {
		let position = self
			.tree
			.find(ROOT_ID, &PRIVATE_DIRECTORY_NAME, query)?
			.ok_or(dangling(id, LinkTarget::PrivateDirectory))?;
		let directory = self.decode_entry(position, query.record(position)?)?;
		if directory.name != PRIVATE_DIRECTORY_NAME
			|| !matches!(directory.kind, EntryKind::Directory { .. })
		{
			return Err(corrupt(id, LinkCorruption::PrivateDirectory));
		}
		self.check_thread(&directory, query)?;
		Ok(directory)
	}

	fn decode_stub(
		&self,
		entry: &Entry,
		query: &mut Query,
	) -> Result<Option<Stub>, HardLinkError<S::Error>> {
		let id = entry.metadata.id;
		let position = self
			.tree
			.find(entry.parent, &entry.name, query)?
			.ok_or(dangling(id, LinkTarget::ChainLink(id)))?;
		let raw = query.record(position)?;
		let record = Record::decode(raw, self.geometry)
			.map_err(|error| super::format(position.node, error))?;
		Stub::decode(&record).map_err(|error| HardLinkError::Codec { id, error })
	}

	fn verify_chain(
		&self,
		selected: &Entry,
		inode: Inode,
		head: CatalogId,
		created: [u32; 2],
		query: &mut Query,
	) -> Result<(), HardLinkError<S::Error>> {
		let id = selected.metadata.id;
		let mut current = Some(head);
		let mut previous = None;
		let mut contains_entry = false;
		for _ in 0..inode.link_count.get() {
			let link = current.ok_or(corrupt(id, LinkCorruption::Chain))?;
			let entry = self
				.resolve_public(link, query)?
				.ok_or(dangling(id, LinkTarget::ChainLink(link)))?;
			let stub = self
				.decode_stub(&entry, query)?
				.filter(|stub| stub.reference == inode.reference)
				.ok_or(corrupt(id, LinkCorruption::Chain))?;
			let neighbors = stub
				.neighbors
				.ok_or(corrupt(id, LinkCorruption::ConflictingProfiles))?;
			if neighbors.previous != previous
				|| !created.contains(&entry.metadata.dates[CREATED_DATE_INDEX])
			{
				return Err(corrupt(id, LinkCorruption::Chain));
			}
			contains_entry |= link == id;
			previous = Some(link);
			current = neighbors.next;
		}
		if current.is_some() || !contains_entry {
			return Err(corrupt(id, LinkCorruption::Chain));
		}
		Ok(())
	}
}

pub(super) fn is_directory_link(entry: &Entry) -> bool {
	entry
		.metadata
		.finder_info
		.starts_with(DIRECTORY_LINK_FINDER_CODE)
}

const fn corrupt<E>(id: CatalogId, reason: LinkCorruption) -> HardLinkError<E> {
	HardLinkError::Corrupt { id, reason }
}

const fn dangling<E>(id: CatalogId, target: LinkTarget) -> HardLinkError<E> {
	HardLinkError::Dangling { id, target }
}

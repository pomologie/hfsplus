use core::fmt;

use hfsplus_core::ReadAt;
use hfsplus_format::{attributes::Key, bsd, volume::CatalogId};

use super::{Catalog, CatalogError, Entry, EntryKind, HardLinkError, HardLinkLimits};
use crate::{
	alloc::vec::Vec,
	attributes::{AttributeEntry, AttributeReadError, AttributeReadLimits, AttributeValue},
};

/// The catalog owners included in an attribute presence query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AttributeScope {
	/// The selected entry and its validated storage inode, if distinct.
	Entry,
	/// Also include directories from the selected entry's parent to the root.
	EntryAndAncestors,
}

/// Evidence obtained for an attribute name in the requested scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AttributePresence {
	/// One owner contains the requested attribute.
	Present,
	/// The attribute was absent from every owner in the scope.
	Absent,
	/// The requested catalog ID is absent or invisible.
	Incomplete,
}

/// A catalog entry's attribute lookup or listing failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum CatalogAttributeError<E> {
	/// The visible catalog entry could not be resolved.
	Catalog(CatalogError<E>),
	/// The regular file's storage identity could not be resolved.
	HardLink(HardLinkError<E>),
	/// The native attribute tree could not satisfy the request.
	Attributes(AttributeReadError<E>),
	/// The visible owner is absent or inaccessible.
	MissingOwner(CatalogId),
	/// A visible regular file is unavailable during storage lookup.
	MissingRegularFile(CatalogId),
	/// The name is stored on both the visible stub and its inode.
	AmbiguousOwner {
		visible: CatalogId,
		inode: CatalogId,
	},
}

impl<E: fmt::Display> fmt::Display for CatalogAttributeError<E> {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Catalog(error) => error.fmt(formatter),
			Self::HardLink(error) => error.fmt(formatter),
			Self::Attributes(error) => error.fmt(formatter),
			Self::MissingOwner(id) => write!(formatter, "missing attribute owner {}", id.0),
			Self::MissingRegularFile(id) => {
				write!(formatter, "missing regular file with catalog ID {}", id.0)
			}
			Self::AmbiguousOwner { visible, inode } => write!(
				formatter,
				"hard link {} and inode {} both own the attribute",
				visible.0, inode.0
			),
		}
	}
}

impl<E: core::error::Error + 'static> core::error::Error for CatalogAttributeError<E> {
	fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
		match self {
			Self::Catalog(error) => Some(error),
			Self::HardLink(error) => Some(error),
			Self::Attributes(error) => Some(error),
			_ => None,
		}
	}
}

impl<E> From<AttributeReadError<E>> for CatalogAttributeError<E> {
	fn from(error: AttributeReadError<E>) -> Self {
		Self::Attributes(error)
	}
}

impl<'volume, S: ReadAt> Catalog<'volume, S> {
	/// Observes whether any owner in the scope has this exact stored name.
	///
	/// A volume with no attributes fork establishes absence directly. Otherwise
	/// the catalog validates the selected entry and its visible ancestor chain.
	/// A regular-file hard link can find a name on its stub or validated inode.
	/// Ancestor lookup always uses the selected entry's path. The query does not
	/// follow symlinks or read fork-backed attribute payloads.
	///
	/// # Errors
	/// Returns errors for malformed names or records, failed reads, or limit exhaustion.
	pub fn observe_entry_attribute(
		&self,
		visible: CatalogId,
		name: &[u16],
		scope: AttributeScope,
		hard_links: HardLinkLimits,
		limits: AttributeReadLimits,
	) -> Result<AttributePresence, CatalogAttributeError<S::Error>> {
		Key::encoded_len(name).map_err(|_| AttributeReadError::InvalidName)?;
		let fork = self.volume.header().forks.attributes;
		if fork.logical_bytes == 0 && fork.total_blocks == 0 {
			return Ok(AttributePresence::Absent);
		}
		let mut query = self.query().map_err(CatalogAttributeError::Catalog)?;
		let Some(selected) = self
			.resolve_public_with_ancestors(visible, &mut query)
			.map_err(CatalogAttributeError::Catalog)?
		else {
			return Ok(AttributePresence::Incomplete);
		};
		let attributes = self.volume.attributes(limits)?;
		let present = |owner| {
			attributes
				.open_attribute(owner, name)
				.map(|value| value.is_some())
		};
		if present(visible)? {
			return Ok(AttributePresence::Present);
		}
		if let Some(inode) = self.attribute_inode_for_entry(&selected.entry, hard_links)?
			&& present(inode)?
		{
			return Ok(AttributePresence::Present);
		}
		if scope == AttributeScope::EntryAndAncestors {
			for ancestor in selected.ancestors {
				if present(ancestor)? {
					return Ok(AttributePresence::Present);
				}
			}
		}
		Ok(AttributePresence::Absent)
	}

	/// Lists distinct stored attribute names in binary UTF-16 order.
	///
	/// For a regular-file hard link this includes names from both the visible
	/// entry and its validated inode. A shared name appears once in the
	/// listing. Opening that name reports the ambiguous owner.
	/// Attribute node limits apply to each owner scan and entry limits apply
	/// to the combined listing.
	///
	/// # Errors
	/// Returns an error for missing owners, source or format failures, and
	/// exhausted catalog, hard-link, or attribute limits.
	pub fn list_entry_attribute_names(
		&self,
		visible: CatalogId,
		hard_links: HardLinkLimits,
		limits: AttributeReadLimits,
	) -> Result<Vec<Vec<u16>>, CatalogAttributeError<S::Error>> {
		let inode = self.attribute_inode(visible, hard_links)?;
		let attributes = self.volume.attributes(limits)?;
		let mut names = Vec::new();
		let mut visit = |entry: AttributeEntry, _| {
			names
				.try_reserve(1)
				.map_err(AttributeReadError::Allocation)?;
			names.push(entry.name);
			Ok::<_, AttributeReadError<S::Error>>(())
		};
		attributes.visit_attrs(visible, &mut visit)?;
		if let Some(inode) = inode {
			attributes.visit_attrs(inode, visit)?;
			names.sort_unstable();
			names.dedup();
			if names.len() > limits.max_entries {
				return Err(AttributeReadError::EntryLimit.into());
			}
		}
		Ok(names)
	}

	/// Opens the stored value with the exact raw UTF-16 name for a visible
	/// catalog entry, including a regular-file hard link's validated inode.
	/// Returns no value when neither owner contains the name.
	///
	/// # Errors
	/// Returns an error for a missing owner, duplicate ownership of the name,
	/// an invalid name or stored representation, failed reads, or limit exhaustion.
	pub fn open_entry_attribute(
		&self,
		visible: CatalogId,
		name: &[u16],
		hard_links: HardLinkLimits,
		limits: AttributeReadLimits,
	) -> Result<Option<AttributeValue<'volume, S>>, CatalogAttributeError<S::Error>> {
		let inode = self.attribute_inode(visible, hard_links)?;
		let attributes = self.volume.attributes(limits)?;
		let value = attributes.open_attribute(visible, name)?;
		let Some(inode) = inode else {
			return Ok(value);
		};
		let stored = attributes.open_attribute(inode, name)?;
		if value.is_some() && stored.is_some() {
			return Err(CatalogAttributeError::AmbiguousOwner { visible, inode });
		}
		Ok(value.or(stored))
	}

	fn attribute_inode(
		&self,
		visible: CatalogId,
		limits: HardLinkLimits,
	) -> Result<Option<CatalogId>, CatalogAttributeError<S::Error>> {
		let entry = self
			.entry(visible)
			.map_err(CatalogAttributeError::Catalog)?
			.ok_or(CatalogAttributeError::MissingOwner(visible))?;
		self.attribute_inode_for_entry(&entry, limits)
	}

	fn attribute_inode_for_entry(
		&self,
		entry: &Entry,
		limits: HardLinkLimits,
	) -> Result<Option<CatalogId>, CatalogAttributeError<S::Error>> {
		if !matches!(entry.kind, EntryKind::File { .. })
			|| entry.metadata.permissions.mode & bsd::TYPE_MASK != bsd::REGULAR_FILE
		{
			return Ok(None);
		}
		let visible = entry.metadata.id;
		let file = self
			.regular_file(visible, limits)
			.map_err(CatalogAttributeError::HardLink)?
			.ok_or(CatalogAttributeError::MissingRegularFile(visible))?;
		let inode = file.inode_id();
		Ok((inode != visible).then_some(inode))
	}
}

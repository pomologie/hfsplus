use core::fmt;

use hfsplus_core::ReadAt;
use hfsplus_format::{bsd, volume::CatalogId};

use super::{Catalog, CatalogError, EntryKind, HardLinkError, HardLinkLimits};
use crate::{
	alloc::vec::Vec,
	attributes::{AttributeEntry, AttributeReadError, AttributeReadLimits, AttributeValue},
};

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
		if !matches!(entry.kind, EntryKind::File { .. })
			|| entry.metadata.permissions.mode & bsd::TYPE_MASK != bsd::REGULAR_FILE
		{
			return Ok(None);
		}
		let file = self
			.regular_file(visible, limits)
			.map_err(CatalogAttributeError::HardLink)?
			.ok_or(CatalogAttributeError::MissingRegularFile(visible))?;
		let inode = file.inode_id();
		Ok((inode != visible).then_some(inode))
	}
}

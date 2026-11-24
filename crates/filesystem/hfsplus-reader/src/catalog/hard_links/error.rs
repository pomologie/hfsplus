use core::fmt;

use hfsplus_format::{
	hard_links::{self, LinkReference},
	volume::CatalogId,
};

use super::super::CatalogError;

/// A required target was absent or inaccessible during link resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LinkTarget {
	Root,
	PrivateDirectory,
	Inode(LinkReference),
	ChainLink(CatalogId),
}

/// A hard-link relationship contradicted the selected catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LinkCorruption {
	PrivateDirectory,
	InodeKey,
	CreationDate,
	ConflictingProfiles,
	Chain,
}

/// A checked regular-file or hard-link lookup failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum HardLinkError<E> {
	/// Catalog traversal failed.
	Catalog(CatalogError<E>),
	/// The on-disk stub or inode did not satisfy the codec's profile.
	Codec {
		id: CatalogId,
		error: hard_links::Error,
	},
	/// The requested visible item is not a regular file.
	NotRegular(CatalogId),
	/// The item uses the directory-link profile.
	DirectoryLink(CatalogId),
	/// A required lookup found no accessible matching catalog entry.
	Dangling { id: CatalogId, target: LinkTarget },
	/// Cross-record identity or chain evidence contradicted the catalog.
	Corrupt {
		id: CatalogId,
		reason: LinkCorruption,
	},
	/// The linked chain exceeds its configured count budget.
	ChainLimit(CatalogId),
}

impl<E> From<CatalogError<E>> for HardLinkError<E> {
	fn from(error: CatalogError<E>) -> Self {
		Self::Catalog(error)
	}
}

impl<E: fmt::Display> fmt::Display for HardLinkError<E> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Catalog(error) => error.fmt(f),
			Self::Codec { id, error } => write!(
				f,
				"invalid hard-link record for catalog ID {}: {error}",
				id.0
			),
			Self::NotRegular(id) => write!(f, "catalog ID {} does not name a regular file", id.0),
			Self::DirectoryLink(id) => write!(
				f,
				"directory hard link at catalog ID {} is unsupported",
				id.0
			),
			Self::Dangling { id, target } => write!(
				f,
				"hard link at catalog ID {} lacks target {target:?}",
				id.0
			),
			Self::Corrupt { id, reason } => write!(
				f,
				"hard link at catalog ID {} failed {reason:?} validation",
				id.0
			),
			Self::ChainLimit(id) => write!(
				f,
				"hard-link chain for catalog ID {} exceeds its limit",
				id.0
			),
		}
	}
}

impl<E: core::error::Error + 'static> core::error::Error for HardLinkError<E> {
	fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
		match self {
			Self::Catalog(error) => Some(error),
			Self::Codec { error, .. } => Some(error),
			_ => None,
		}
	}
}

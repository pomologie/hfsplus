use alloc::collections::TryReserveError;
use core::fmt;

use hfsplus_core::names::NameError;
use hfsplus_format::{
	btree::NodeId,
	volume::{CatalogId, Signature},
};

use crate::Error;

/// A bounded catalog operation failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum CatalogError<E> {
	/// The source or the catalog fork could not be read.
	Read(Error<E>),
	/// The catalog record or node failed format decoding.
	Format {
		node: NodeId,
		error: hfsplus_format::Error,
	},
	/// The parsed structure violated a catalog traversal invariant.
	Corrupt { node: NodeId, reason: Corruption },
	/// The volume uses a different filesystem profile.
	UnsupportedVolume(Signature),
	/// The tree uses an unqualified key layout or type.
	UnsupportedTree,
	/// An explicit operation budget was exceeded.
	Limit(Limit),
	/// Allocation failed within the configured limit.
	Allocation(TryReserveError),
	/// Named lookup input could not be normalized.
	Name(NameError),
	/// Child lookup requires a nonempty name within the catalog key limit.
	InvalidName,
	/// Catalog ID is absent, private, or not a directory.
	ExpectedDirectory(CatalogId),
}

/// Work limits enforced by the catalog reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Limit {
	Nodes,
	Depth,
	AllocationMap,
	Ancestors,
	PageEntries,
}

/// Catalog consistency checks that can fail during a query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Corruption {
	AllocationMap,
	NodeAddress,
	NodeKind,
	NodeLinks,
	RecordOrder,
	IndexRoute,
	MissingRoot,
	Thread,
	Parent,
	ParentCycle,
	Valence,
}

impl<E: fmt::Display> fmt::Display for CatalogError<E> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Read(error) => error.fmt(f),
			Self::Format { node, error } => write!(f, "catalog node {}: {error}", node.0),
			Self::Corrupt { node, reason } => {
				write!(f, "catalog node {} failed {reason:?} validation", node.0)
			}
			Self::UnsupportedVolume(signature) => {
				write!(f, "unsupported catalog volume profile {signature:?}")
			}
			Self::UnsupportedTree => f.write_str("unsupported catalog B-tree layout or type"),
			Self::Limit(limit) => write!(f, "catalog operation exceeds the {limit:?} limit"),
			Self::Allocation(error) => write!(f, "cannot allocate catalog data: {error}"),
			Self::Name(error) => error.fmt(f),
			Self::InvalidName => f.write_str("invalid child name for catalog lookup"),
			Self::ExpectedDirectory(id) => write!(
				f,
				"catalog ID {} does not name an accessible directory",
				id.0
			),
		}
	}
}

impl<E: core::error::Error + 'static> core::error::Error for CatalogError<E> {
	fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
		match self {
			Self::Read(error) => Some(error),
			Self::Format { error, .. } => Some(error),
			Self::Allocation(error) => Some(error),
			Self::Name(error) => Some(error),
			_ => None,
		}
	}
}

use alloc::collections::TryReserveError;
use core::fmt;

use hfsplus_format::{hard_links::LinkReference, volume::CatalogId};

use super::AllocationOwner;
use crate::{
	allocation::AllocationError,
	attributes::AttributeReadError,
	catalog::{CatalogError, HardLinkError},
	overflow::OverflowError,
};

/// Work bounds reached by the structural audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CheckLimit {
	CatalogEntries,
	ParentDepth,
	AttributeNames,
}

/// Inconsistent relationships among structurally readable catalog records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CatalogProblem {
	MissingRoot,
	DuplicateId(CatalogId),
	Parent {
		child: CatalogId,
		parent: CatalogId,
	},
	ParentCycle(CatalogId),
	Valence {
		id: CatalogId,
		declared: u32,
		actual: u32,
	},
	LeafRecords {
		declared: u32,
		actual: u32,
	},
	ThreadCount {
		records: u32,
		threads: u32,
	},
	FileCount {
		declared: u32,
		actual: u32,
	},
	FolderCount {
		declared: u32,
		actual: u32,
	},
	LinkCount {
		reference: LinkReference,
		declared: u32,
		actual: u32,
	},
	LinkInode(CatalogId),
	MissingAttributes(CatalogId),
}

/// A structural audit failed before producing a report.
#[derive(Debug)]
#[non_exhaustive]
pub enum CheckError<E> {
	Allocation(AllocationError<E, AllocationOwner>),
	Catalog(CatalogError<E>),
	HardLink(HardLinkError<E>),
	Attributes(AttributeReadError<E>),
	Fork {
		owner: AllocationOwner,
		error: OverflowError<E>,
	},
	Structure(CatalogProblem),
	Limit(CheckLimit),
	Memory(TryReserveError),
}

impl<E> From<AllocationError<E, AllocationOwner>> for CheckError<E> {
	fn from(error: AllocationError<E, AllocationOwner>) -> Self {
		Self::Allocation(error)
	}
}

impl<E> From<CatalogError<E>> for CheckError<E> {
	fn from(error: CatalogError<E>) -> Self {
		Self::Catalog(error)
	}
}

impl<E> From<HardLinkError<E>> for CheckError<E> {
	fn from(error: HardLinkError<E>) -> Self {
		Self::HardLink(error)
	}
}

impl<E> From<CatalogProblem> for CheckError<E> {
	fn from(error: CatalogProblem) -> Self {
		Self::Structure(error)
	}
}

impl<E: fmt::Display> fmt::Display for CheckError<E> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Allocation(error) => error.fmt(f),
			Self::Catalog(error) => error.fmt(f),
			Self::HardLink(error) => error.fmt(f),
			Self::Attributes(error) => error.fmt(f),
			Self::Fork { owner, error } => write!(f, "fork for {owner:?} failed: {error}"),
			Self::Structure(problem) => write!(f, "catalog structure invalid: {problem:?}"),
			Self::Limit(limit) => write!(f, "structural check reached the {limit:?} limit"),
			Self::Memory(error) => write!(f, "cannot allocate structural check state: {error}"),
		}
	}
}

impl<E: core::error::Error + 'static> core::error::Error for CheckError<E> {
	fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
		match self {
			Self::Allocation(error) => Some(error),
			Self::Catalog(error) => Some(error),
			Self::HardLink(error) => Some(error),
			Self::Attributes(error) => Some(error),
			Self::Fork { error, .. } => Some(error),
			Self::Memory(error) => Some(error),
			_ => None,
		}
	}
}

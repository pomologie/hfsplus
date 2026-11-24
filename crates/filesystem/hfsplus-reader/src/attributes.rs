//! Bounded reads of the native HFS+ inline attribute tree.

use alloc::{collections::TryReserveError, vec::Vec};
use core::{fmt, ops::ControlFlow};

use hfsplus_core::{ByteLen, ReadAt};
use hfsplus_format::{
	Error as FormatError,
	attributes::{InlineRecord, Key},
	btree::NodeId,
	volume::CatalogId,
};

use crate::{Error as VolumeError, SpecialFile, Volume};

mod tree;

use tree::{Query, Tree};

const FIRST_ATTRIBUTE_BLOCK: u32 = 0;
const FORK_ATTRIBUTE_RECORD: u32 = 0x20;
const EXTENTS_ATTRIBUTE_RECORD: u32 = 0x30;
const DEFAULT_MAX_NODES: usize = 64;
const DEFAULT_MAX_ENTRIES: usize = 1024;

/// Resource limits for one attribute lookup or listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttributeReadLimits {
	/// Maximum index and leaf nodes read per call, excluding the tree header.
	pub max_nodes: usize,
	/// Maximum distinct attribute names returned by one listing.
	pub max_entries: usize,
}

impl Default for AttributeReadLimits {
	fn default() -> Self {
		Self {
			max_nodes: DEFAULT_MAX_NODES,
			max_entries: DEFAULT_MAX_ENTRIES,
		}
	}
}

/// A logical attribute and the readability of its stored representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributeEntry {
	/// Exact UTF-16 name units in binary order, without catalog normalization.
	pub name: Vec<u16>,
	/// Inline value length or an unsupported representation reported by the decoder.
	pub encoding: AttributeEncoding,
}

/// Value representation observed for a logical attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AttributeEncoding {
	/// Exact byte length of the inline value, excluding alignment padding.
	Inline(ByteLen),
	/// Inline value outside the qualified size profile.
	UnsupportedInlineSize(usize),
	/// The attribute controls compressed file data.
	CompressedFile,
	/// Unsupported on-disk record kind, including fork-backed attributes.
	UnsupportedRecord(u32),
}

/// An attribute scan failed before it could return a complete result.
#[derive(Debug)]
#[non_exhaustive]
pub enum AttributeReadError<E> {
	/// Attribute fork access or the underlying source failed.
	Fork(VolumeError<E>),
	/// Unsupported size, key layout, or allocation map shape for this tree.
	UnsupportedTree,
	/// An attribute lookup name has an invalid length.
	InvalidName,
	/// A format error in a particular attribute tree node.
	Format {
		/// The node within the attributes fork.
		node: NodeId,
		/// The format error, including typed unsupported attribute representations.
		error: FormatError,
	},
	/// The operation reached its node limit.
	NodeLimit,
	/// The listing reached its name limit.
	EntryLimit,
	/// The bounded request could not allocate working memory or result storage.
	Allocation(TryReserveError),
}

impl<E> AttributeReadError<E> {
	fn format(node: NodeId, error: FormatError) -> Self {
		Self::Format { node, error }
	}
}

impl<E: fmt::Display> fmt::Display for AttributeReadError<E> {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Fork(error) => error.fmt(formatter),
			Self::UnsupportedTree => formatter.write_str("unsupported attributes tree layout"),
			Self::InvalidName => formatter.write_str("invalid attribute name length"),
			Self::Format { node, error } => {
				write!(formatter, "attribute tree node {}: {error}", node.0)
			}
			Self::NodeLimit => formatter.write_str("attribute tree node limit exceeded"),
			Self::EntryLimit => formatter.write_str("attribute listing limit exceeded"),
			Self::Allocation(error) => error.fmt(formatter),
		}
	}
}

impl<E: core::error::Error + 'static> core::error::Error for AttributeReadError<E> {
	fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
		match self {
			Self::Fork(error) => Some(error),
			Self::Format { error, .. } => Some(error),
			Self::Allocation(error) => Some(error),
			Self::UnsupportedTree | Self::InvalidName | Self::NodeLimit | Self::EntryLimit => None,
		}
	}
}

/// Attribute access bound to one immutable volume view.
#[derive(Debug)]
pub struct Attributes<'volume, S> {
	tree: Option<Tree<'volume, S>>,
	limits: AttributeReadLimits,
}

impl<S: ReadAt> Volume<S> {
	/// Opens the attributes fork and validates the qualified native header and
	/// its node allocation map. A volume with no attributes fork has no names.
	/// This profile supports 8192-byte trees with an allocation map in the
	/// header node and an attributes fork covered by embedded extents.
	/// # Errors
	/// Returns errors for unreadable, malformed, or unsupported volume or tree layouts.
	pub fn attributes(
		&self,
		limits: AttributeReadLimits,
	) -> Result<Attributes<'_, S>, AttributeReadError<S::Error>> {
		let fork = self
			.open_special_fork(SpecialFile::Attributes)
			.map_err(AttributeReadError::Fork)?;
		let tree = if fork.is_empty() {
			None
		} else {
			Some(Tree::open(fork)?)
		};
		Ok(Attributes { tree, limits })
	}
}

impl<S: ReadAt> Attributes<'_, S> {
	/// Lists every distinct attribute on `owner`, preserving binary name order.
	/// Unsupported values are listed with their observed encoding.
	/// # Errors
	/// Returns an error for corrupt keys, failed I/O, or exhaustion of a read limit.
	pub fn list_attrs(
		&self,
		owner: CatalogId,
	) -> Result<Vec<AttributeEntry>, AttributeReadError<S::Error>> {
		let mut entries = Vec::new();
		let Some(tree) = &self.tree else {
			return Ok(entries);
		};
		tree.scan(
			Query { owner, name: &[] },
			self.limits.max_nodes,
			|node, key, raw| {
				if key.file_id != owner {
					return Ok(ControlFlow::Break(()));
				}
				if key.start_block != FIRST_ATTRIBUTE_BLOCK {
					check_continuation(entries.last(), node, key, raw)?;
					return Ok(ControlFlow::Continue(()));
				}
				if entries.len() >= self.limits.max_entries {
					return Err(AttributeReadError::EntryLimit);
				}
				let encoding = match InlineRecord::decode(raw) {
					Ok(record) => AttributeEncoding::Inline(ByteLen::from(record.value.len())),
					Err(FormatError::UnsupportedAttributeRecord(kind)) => {
						AttributeEncoding::UnsupportedRecord(kind)
					}
					Err(FormatError::UnsupportedAttributeSize(size)) => {
						AttributeEncoding::UnsupportedInlineSize(size)
					}
					Err(FormatError::UnsupportedCompressedAttribute) => {
						AttributeEncoding::CompressedFile
					}
					Err(error) => return Err(AttributeReadError::format(node, error)),
				};
				let units = key.name.units();
				let mut name = Vec::new();
				name.try_reserve_exact(units.len())
					.map_err(AttributeReadError::Allocation)?;
				name.extend(units);
				entries
					.try_reserve(1)
					.map_err(AttributeReadError::Allocation)?;
				entries.push(AttributeEntry { name, encoding });
				Ok(ControlFlow::Continue(()))
			},
		)?;
		Ok(entries)
	}

	/// Reads the exact inline value for a raw UTF-16 name. A present empty value
	/// is `Some` with an empty vector. A missing attribute is `None`.
	/// # Errors
	/// Returns an error for invalid names, changed or incomplete source reads,
	/// corrupt trees, or unsupported records such as fork-backed attributes and
	/// compression metadata.
	pub fn open_attr(
		&self,
		owner: CatalogId,
		name: &[u16],
	) -> Result<Option<Vec<u8>>, AttributeReadError<S::Error>> {
		Key::encoded_len(name).map_err(|_| AttributeReadError::InvalidName)?;
		let Some(tree) = &self.tree else {
			return Ok(None);
		};
		let mut result = None;
		tree.scan(
			Query { owner, name },
			self.limits.max_nodes,
			|node, key, raw| {
				if key.file_id != owner || key.name.units().ne(name.iter().copied()) {
					return Ok(ControlFlow::Break(()));
				}
				if key.start_block != FIRST_ATTRIBUTE_BLOCK {
					return Err(AttributeReadError::format(
						node,
						FormatError::InvalidAttribute,
					));
				}
				let record = InlineRecord::decode(raw)
					.map_err(|error| AttributeReadError::format(node, error))?;
				let mut bytes = Vec::new();
				bytes
					.try_reserve_exact(record.value.len())
					.map_err(AttributeReadError::Allocation)?;
				bytes.extend_from_slice(record.value);
				result = Some(bytes);
				Ok(ControlFlow::Continue(()))
			},
		)?;
		Ok(result)
	}
}

fn check_continuation<E>(
	previous: Option<&AttributeEntry>,
	node: NodeId,
	key: Key<'_>,
	raw: &[u8],
) -> Result<(), AttributeReadError<E>> {
	let follows_fork = previous.is_some_and(|entry| {
		entry.encoding == AttributeEncoding::UnsupportedRecord(FORK_ATTRIBUTE_RECORD)
			&& key.name.units().eq(entry.name.iter().copied())
	});
	if follows_fork
		&& InlineRecord::decode(raw)
			== Err(FormatError::UnsupportedAttributeRecord(
				EXTENTS_ATTRIBUTE_RECORD,
			)) {
		Ok(())
	} else {
		Err(AttributeReadError::format(
			node,
			FormatError::InvalidAttribute,
		))
	}
}

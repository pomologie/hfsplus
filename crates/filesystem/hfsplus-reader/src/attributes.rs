//! Bounded stored-value reads of the native HFS+ attribute tree.

use crate::alloc::{collections::TryReserveError, vec::Vec};
use core::{fmt, ops::ControlFlow};

use hfsplus_core::{ByteLen, ByteOffset, ReadAt};
use hfsplus_format::{
	Error as FormatError,
	attributes::{Key, Record, Value},
	btree::NodeId,
	volume::{CatalogId, Geometry},
};

use crate::{Error as VolumeError, SpecialFile, Volume, overflow::OverflowError};

mod inventory;
mod tree;
mod value;

use tree::{Query, Tree};
use value::{ForkChain, Storage};

pub use value::AttributeValue;

const FIRST_ATTRIBUTE_BLOCK: u32 = 0;
const DEFAULT_MAX_NODES: usize = 64;
const DEFAULT_MAX_ENTRIES: usize = 1024;
/// Maximum owned value size returned by [`Attributes::open_attr`].
pub const MAX_OWNED_ATTRIBUTE_BYTES: ByteLen = ByteLen(1 << 20);

/// Resource limits for one attribute lookup or listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttributeReadLimits {
	/// Maximum index and leaf nodes read per call, excluding the tree header.
	pub max_nodes: usize,
	/// Maximum distinct attribute names in one listing or complete inventory.
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
	/// Stored value length or an unsupported representation reported by the decoder.
	pub encoding: AttributeEncoding,
}

/// Value representation observed for a logical attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AttributeEncoding {
	/// Exact byte length of the inline value, excluding alignment padding.
	Inline(ByteLen),
	/// Exact logical byte length of a validated extent-backed value.
	Fork(ByteLen),
	/// Inline value outside the qualified size profile.
	UnsupportedInlineSize(usize),
	/// The attribute controls compressed file data.
	CompressedFile,
	/// Unsupported on-disk record kind.
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
	/// A complete inline inventory encountered fork-backed attributes.
	NonInline { node: NodeId },
	/// A continuation begins at the wrong logical allocation block.
	InvalidStart {
		node: NodeId,
		expected: u32,
		actual: u32,
	},
	/// Allocation records ended before the declared fork allocation was covered.
	IncompleteFork {
		node: NodeId,
		covered: u32,
		declared: u32,
	},
	/// The assembled fork contains an invalid physical mapping.
	Layout {
		node: NodeId,
		error: OverflowError<E>,
	},
	/// A value exceeds the convenience method's owned byte limit.
	ValueLimit { bytes: ByteLen },
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
			Self::NonInline { node } => {
				write!(
					formatter,
					"attribute tree node {} contains a fork-backed value",
					node.0
				)
			}
			Self::InvalidStart {
				node,
				expected,
				actual,
			} => write!(
				formatter,
				"attribute node {} starts at block {actual}, expected {expected}",
				node.0
			),
			Self::IncompleteFork {
				node,
				covered,
				declared,
			} => write!(
				formatter,
				"attribute at node {} covers {covered} of {declared} blocks",
				node.0
			),
			Self::Layout { node, error } => {
				write!(formatter, "attribute fork at node {}: {error}", node.0)
			}
			Self::ValueLimit { bytes } => write!(
				formatter,
				"attribute value of {} bytes exceeds the owned read limit",
				bytes.0
			),
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
			Self::Layout { error, .. } => Some(error),
			Self::Allocation(error) => Some(error),
			_ => None,
		}
	}
}

/// Attribute access bound to one immutable volume view.
#[derive(Debug)]
pub struct Attributes<'volume, S> {
	volume: &'volume Volume<S>,
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
		Ok(Attributes {
			volume: self,
			tree,
			limits,
		})
	}
}

impl<'volume, S: ReadAt> Attributes<'volume, S> {
	/// Lists every distinct attribute on `owner`, preserving binary name order.
	/// Validates each fork's allocation chain without reading its payload.
	/// Unsupported values are listed with their observed encoding.
	/// # Errors
	/// Returns an error for corrupt keys or chains, failed I/O, or read limits.
	pub fn list_attrs(
		&self,
		owner: CatalogId,
	) -> Result<Vec<AttributeEntry>, AttributeReadError<S::Error>> {
		let mut entries = Vec::new();
		self.visit_attrs::<AttributeReadError<S::Error>>(owner, |entry, _| {
			entries
				.try_reserve(1)
				.map_err(AttributeReadError::Allocation)?;
			entries.push(entry);
			Ok(())
		})?;
		Ok(entries)
	}

	/// Visits logical keys in binary order, with an opened value for each
	/// fork-backed entry after its complete extent chain is validated.
	pub(crate) fn visit_attrs<E: From<AttributeReadError<S::Error>>>(
		&self,
		owner: CatalogId,
		mut visit: impl FnMut(AttributeEntry, Option<AttributeValue<'volume, S>>) -> Result<(), E>,
	) -> Result<(), E> {
		let Some(tree) = &self.tree else {
			return Ok(());
		};
		let geometry = self.volume.header().geometry;
		let mut pending: Option<(AttributeEntry, Option<ForkChain>)> = None;
		let mut count = 0;
		let mut finish = |(entry, chain): (AttributeEntry, Option<ForkChain>)| {
			let fork = chain
				.map(|chain| chain.finish(self.volume))
				.transpose()?
				.map(|fork| AttributeValue {
					value: Storage::Fork(fork),
				});
			visit(entry, fork)
		};
		tree.scan::<E>(
			Query { owner, name: &[] },
			self.limits.max_nodes,
			|node, key, raw| {
				if key.file_id != owner {
					return Ok(ControlFlow::Break(()));
				}
				if key.start_block != FIRST_ATTRIBUTE_BLOCK {
					let (_, chain) = pending
						.as_mut()
						.filter(|(entry, _)| key.name.units().eq(entry.name.iter().copied()))
						.ok_or_else(|| {
							AttributeReadError::format(node, FormatError::InvalidAttribute)
						})?;
					append_continuation(chain.as_mut(), node, raw, geometry)?;
					return Ok(ControlFlow::Continue(()));
				}
				if let Some(previous) = pending.take() {
					finish(previous)?;
				}
				if count >= self.limits.max_entries {
					return Err(AttributeReadError::EntryLimit.into());
				}
				count += 1;
				let mut chain = None;
				let encoding = match Record::decode(raw, geometry).map(|record| record.value) {
					Ok(Value::Inline(bytes)) => {
						AttributeEncoding::Inline(ByteLen::from(bytes.len()))
					}
					Ok(Value::Fork(fork)) => {
						chain = Some(ForkChain::new(node, fork, geometry)?);
						AttributeEncoding::Fork(ByteLen(fork.logical_bytes))
					}
					Ok(_) => {
						return Err(AttributeReadError::format(
							node,
							FormatError::InvalidAttribute,
						)
						.into());
					}
					Err(FormatError::UnsupportedAttributeRecord(kind)) => {
						AttributeEncoding::UnsupportedRecord(kind)
					}
					Err(FormatError::UnsupportedAttributeSize(size)) => {
						AttributeEncoding::UnsupportedInlineSize(size)
					}
					Err(FormatError::UnsupportedCompressedAttribute) => {
						AttributeEncoding::CompressedFile
					}
					Err(error) => return Err(AttributeReadError::format(node, error).into()),
				};
				let units = key.name.units();
				let mut name = Vec::new();
				name.try_reserve_exact(units.len())
					.map_err(AttributeReadError::Allocation)?;
				name.extend(units);
				pending = Some((AttributeEntry { name, encoding }, chain));
				Ok(ControlFlow::Continue(()))
			},
		)?;
		if let Some(previous) = pending {
			finish(previous)?;
		}
		Ok(())
	}

	/// Reads the exact stored value within [`MAX_OWNED_ATTRIBUTE_BYTES`]. A
	/// present empty value returns `Some` with an empty vector. Use
	/// [`Self::open_attribute`] to read larger values by logical range.
	/// # Errors
	/// Returns an error for unreadable metadata or payloads, unsupported
	/// representations, or values exceeding the owned read limit.
	pub fn open_attr(
		&self,
		owner: CatalogId,
		name: &[u16],
	) -> Result<Option<Vec<u8>>, AttributeReadError<S::Error>> {
		let Some(value) = self.open_attribute(owner, name)? else {
			return Ok(None);
		};
		let bytes = value.len();
		if bytes.0 > MAX_OWNED_ATTRIBUTE_BYTES.0 {
			return Err(AttributeReadError::ValueLimit { bytes });
		}
		let result = match value.value {
			Storage::Inline(bytes) => bytes,
			Storage::Fork(fork) => {
				let len = usize::try_from(bytes.0)
					.map_err(|_| AttributeReadError::ValueLimit { bytes })?;
				let mut result = Vec::new();
				result
					.try_reserve_exact(len)
					.map_err(AttributeReadError::Allocation)?;
				result.resize(len, 0);
				fork.read_exact_at(ByteOffset(0), &mut result)
					.map_err(AttributeReadError::Fork)?;
				result
			}
		};
		Ok(Some(result))
	}

	/// Opens a validated inline or fork-backed value by its raw UTF-16 name.
	/// A missing attribute returns `None`. Fork payloads are read on demand.
	/// # Errors
	/// Rejects invalid names, source and range failures, malformed extent chains,
	/// exhausted limits, and unsupported forms such as compression metadata.
	pub fn open_attribute(
		&self,
		owner: CatalogId,
		name: &[u16],
	) -> Result<Option<AttributeValue<'volume, S>>, AttributeReadError<S::Error>> {
		Key::encoded_len(name).map_err(|_| AttributeReadError::InvalidName)?;
		let Some(tree) = &self.tree else {
			return Ok(None);
		};
		let geometry = self.volume.header().geometry;
		let mut inline = None;
		let mut chain = None;
		tree.scan::<AttributeReadError<S::Error>>(
			Query { owner, name },
			self.limits.max_nodes,
			|node, key, raw| {
				if key.file_id != owner || key.name.units().ne(name.iter().copied()) {
					return Ok(ControlFlow::Break(()));
				}
				if key.start_block != FIRST_ATTRIBUTE_BLOCK {
					append_continuation(chain.as_mut(), node, raw, geometry)?;
					return Ok(ControlFlow::Continue(()));
				}
				let record = Record::decode(raw, geometry)
					.map_err(|error| AttributeReadError::format(node, error))?;
				match record.value {
					Value::Inline(bytes) => inline = Some(copy_inline(bytes)?),
					Value::Fork(fork) => chain = Some(ForkChain::new(node, fork, geometry)?),
					_ => {
						return Err(AttributeReadError::format(
							node,
							FormatError::InvalidAttribute,
						));
					}
				}
				Ok(ControlFlow::Continue(()))
			},
		)?;
		let result = if let Some(fork) = chain {
			Some(Storage::Fork(fork.finish(self.volume)?))
		} else {
			inline.map(Storage::Inline)
		};
		Ok(result.map(|value| AttributeValue { value }))
	}

	#[cfg(feature = "decmpfs-zlib")]
	pub(crate) fn read_compression_metadata(
		&self,
		owner: CatalogId,
	) -> Result<Option<Vec<u8>>, AttributeReadError<S::Error>> {
		use hfsplus_format::attributes::{COMPRESSED_ATTRIBUTE_NAME, InlineRecord};

		let Some(tree) = &self.tree else {
			return Ok(None);
		};
		let mut name = [0; COMPRESSED_ATTRIBUTE_NAME.len()];
		for (unit, byte) in name.iter_mut().zip(COMPRESSED_ATTRIBUTE_NAME.bytes()) {
			*unit = u16::from(byte);
		}
		let mut result = None;
		tree.scan::<AttributeReadError<S::Error>>(
			Query { owner, name: &name },
			self.limits.max_nodes,
			|node, key, raw| {
				if key.file_id != owner || key.name.units().ne(name) {
					return Ok(ControlFlow::Break(()));
				}
				let record = InlineRecord::decode_compression_metadata(raw)
					.map_err(|error| AttributeReadError::format(node, error))?;
				result = Some(copy_inline(record.value)?);
				Ok(ControlFlow::Continue(()))
			},
		)?;
		Ok(result)
	}
}

fn append_continuation<E>(
	chain: Option<&mut ForkChain>,
	node: NodeId,
	raw: &[u8],
	geometry: Geometry,
) -> Result<(), AttributeReadError<E>> {
	let invalid = || AttributeReadError::format(node, FormatError::InvalidAttribute);
	let chain = chain.ok_or_else(invalid)?;
	let record =
		Record::decode(raw, geometry).map_err(|error| AttributeReadError::format(node, error))?;
	let Value::Extents(group) = record.value else {
		return Err(invalid());
	};
	chain.append(node, record.key.start_block, group)
}

fn copy_inline<E>(value: &[u8]) -> Result<Vec<u8>, AttributeReadError<E>> {
	let mut bytes = Vec::new();
	bytes
		.try_reserve_exact(value.len())
		.map_err(AttributeReadError::Allocation)?;
	bytes.extend_from_slice(value);
	Ok(bytes)
}

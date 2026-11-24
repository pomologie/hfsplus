//! Allocation-free structural codecs for packed HFS+ B-tree nodes.

mod encode;

use crate::{
	Error,
	binary::{u16_at, u32_at},
};

pub use encode::{NodeLinks, TreeLayout, TreeMetadata, TreeTopology};

/// Width of the first record in a B-tree header node.
pub const HEADER_RECORD_BYTES: usize = 106;
/// Width of the user data record in a B-tree header node.
pub const HEADER_USER_DATA_BYTES: usize = 128;
/// Tree type for the catalog, extent overflow and attributes trees.
pub const HFS_BTREE_TYPE: u8 = 0;
/// Header attribute for the two-byte key length used in HFS+ trees.
pub const BIG_KEYS_MASK: u32 = 0x0000_0002;
/// Header attribute for variable-length keys in index records.
pub const VARIABLE_INDEX_KEYS_MASK: u32 = 0x0000_0004;
/// Header comparator code for case-insensitive catalogs.
pub const CASE_FOLDING_COMPARE: u8 = 0xcf;
/// Header comparator code for case-sensitive HFSX catalogs.
pub const BINARY_COMPARE: u8 = 0xbc;

const MIN_NODE_BYTES: usize = 512;
const MAX_NODE_BYTES: usize = 32_768;
const OFFSET_BYTES: usize = size_of::<u16>();
const RECORD_ALIGNMENT_BYTES: usize = 2;
const FREE_SPACE_OFFSET_COUNT: usize = 1;
const NULL_NODE_ID: u32 = 0;
const EMPTY_TREE_DEPTH: u16 = 0;
/// Descriptor height for header and allocation map nodes.
pub const METADATA_NODE_HEIGHT: u8 = 0;
/// Descriptor height for leaf nodes.
pub const LEAF_HEIGHT: u8 = 1;
/// Lowest descriptor height for index nodes.
pub const MIN_INDEX_HEIGHT: u8 = LEAF_HEIGHT + 1;
const MIN_KEYS_PER_NODE: u16 = 2;

mod descriptor {
	pub(super) const BYTES: usize = 14;
	pub(super) const FORWARD_LINK: usize = 0;
	pub(super) const BACKWARD_LINK: usize = 4;
	pub(super) const KIND: usize = 8;
	pub(super) const HEIGHT: usize = 9;
	pub(super) const RECORD_COUNT: usize = 10;

	pub(super) const LEAF_KIND: u8 = 0xff;
	pub(super) const INDEX_KIND: u8 = 0;
	pub(super) const HEADER_KIND: u8 = 1;
	pub(super) const MAP_KIND: u8 = 2;
	pub(super) const HEADER_RECORD_COUNT: usize = 3;
	pub(super) const MAP_RECORD_COUNT: usize = 1;
}

mod header_record {
	pub(super) const ORDINAL: usize = 0;
	pub(super) const DEPTH: usize = 0;
	pub(super) const ROOT_NODE: usize = 2;
	pub(super) const LEAF_RECORD_COUNT: usize = 6;
	pub(super) const FIRST_LEAF: usize = 10;
	pub(super) const LAST_LEAF: usize = 14;
	pub(super) const NODE_BYTES: usize = 18;
	pub(super) const MAX_KEY_BYTES: usize = 20;
	pub(super) const TOTAL_NODES: usize = 22;
	pub(super) const FREE_NODES: usize = 26;
	pub(super) const CLUMP_BYTES: usize = 32;
	pub(super) const TREE_TYPE: usize = 36;
	pub(super) const KEY_COMPARE_TYPE: usize = 37;
	pub(super) const ATTRIBUTES: usize = 38;
}

/// A B-tree node ID within one fork, with zero used as the null link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub u32);

/// Record schema associated with the node descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
	/// Leaf records with values.
	Leaf = descriptor::LEAF_KIND,
	/// Keyed child references.
	Index = descriptor::INDEX_KIND,
	/// Tree header, reserved record and allocation map.
	Header = descriptor::HEADER_KIND,
	/// Continuation of the allocation map.
	Map = descriptor::MAP_KIND,
}

/// A structurally validated node view. Key order, bitmap and link traversal are
/// checked by the owning tree reader, which has the necessary tree context.
#[derive(Debug)]
pub struct Node<'bytes> {
	bytes: &'bytes [u8],
	kind: Kind,
	height: u8,
	count: usize,
	forward: NodeId,
	backward: NodeId,
}

impl<'bytes> Node<'bytes> {
	/// Decode a complete node and validate its record boundaries.
	///
	/// # Errors
	/// Rejects unsupported node sizes, inconsistent descriptors and invalid record ranges.
	pub fn decode(bytes: &'bytes [u8]) -> Result<Self, Error> {
		if !valid_node_size(bytes.len()) {
			return Err(Error::InvalidNode);
		}
		let kind = match bytes[descriptor::KIND] {
			descriptor::LEAF_KIND => Kind::Leaf,
			descriptor::INDEX_KIND => Kind::Index,
			descriptor::HEADER_KIND => Kind::Header,
			descriptor::MAP_KIND => Kind::Map,
			_ => return Err(Error::InvalidNode),
		};
		let height = bytes[descriptor::HEIGHT];
		let count = usize::from(u16_at(bytes, descriptor::RECORD_COUNT)?);
		let backward = NodeId(u32_at(bytes, descriptor::BACKWARD_LINK)?);
		let table_start = offset_table_start(bytes.len(), count)?;
		if !valid_descriptor(kind, height, count, backward)
			|| record_offset(bytes, 0) != descriptor::BYTES
		{
			return Err(Error::InvalidNode);
		}
		let mut previous = descriptor::BYTES;
		for ordinal in 1..=count {
			let current = record_offset(bytes, ordinal);
			if current <= previous
				|| current > table_start
				|| !current.is_multiple_of(RECORD_ALIGNMENT_BYTES)
			{
				return Err(Error::InvalidNode);
			}
			previous = current;
		}
		Ok(Self {
			bytes,
			kind,
			height,
			count,
			forward: NodeId(u32_at(bytes, descriptor::FORWARD_LINK)?),
			backward,
		})
	}

	/// Format kind of this node.
	#[must_use]
	pub fn kind(&self) -> Kind {
		self.kind
	}

	/// Height from the leaf level, or zero for header/map nodes.
	#[must_use]
	pub fn height(&self) -> u8 {
		self.height
	}

	/// Optional next node in the chain for this level.
	#[must_use]
	pub fn forward(&self) -> Option<NodeId> {
		optional_id(self.forward.0)
	}

	/// Optional previous node in the chain for this level.
	#[must_use]
	pub fn backward(&self) -> Option<NodeId> {
		optional_id(self.backward.0)
	}

	/// Borrow one complete record by its zero-based ordinal.
	#[must_use]
	pub fn record(&self, ordinal: usize) -> Option<&'bytes [u8]> {
		if ordinal >= self.count {
			return None;
		}
		Some(
			&self.bytes[record_offset(self.bytes, ordinal)..record_offset(self.bytes, ordinal + 1)],
		)
	}

	/// Borrow every record in ascending node order.
	pub fn records(&self) -> impl Iterator<Item = &'bytes [u8]> + '_ {
		(0..self.count).map(|ordinal| {
			&self.bytes[record_offset(self.bytes, ordinal)..record_offset(self.bytes, ordinal + 1)]
		})
	}
}

fn valid_node_size(bytes: usize) -> bool {
	(MIN_NODE_BYTES..=MAX_NODE_BYTES).contains(&bytes) && bytes.is_power_of_two()
}

fn valid_descriptor(kind: Kind, height: u8, count: usize, backward: NodeId) -> bool {
	match kind {
		Kind::Leaf => height == LEAF_HEIGHT && count != 0,
		Kind::Index => height >= MIN_INDEX_HEIGHT && count != 0,
		Kind::Header => {
			height == METADATA_NODE_HEIGHT
				&& count == descriptor::HEADER_RECORD_COUNT
				&& backward == NodeId(NULL_NODE_ID)
		}
		Kind::Map => height == METADATA_NODE_HEIGHT && count == descriptor::MAP_RECORD_COUNT,
	}
}

fn offset_table_start(node_bytes: usize, record_count: usize) -> Result<usize, Error> {
	record_count
		.checked_add(FREE_SPACE_OFFSET_COUNT)
		.and_then(|count| count.checked_mul(OFFSET_BYTES))
		.and_then(|width| node_bytes.checked_sub(width))
		.filter(|&start| start >= descriptor::BYTES)
		.ok_or(Error::InvalidNode)
}

fn record_offset_position(node_bytes: usize, ordinal: usize) -> usize {
	node_bytes - (ordinal + 1) * OFFSET_BYTES
}

fn record_offset(bytes: &[u8], ordinal: usize) -> usize {
	let start = record_offset_position(bytes.len(), ordinal);
	usize::from(u16::from_be_bytes([bytes[start], bytes[start + 1]]))
}

/// Geometry and root information from a tree's first node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct TreeHeader {
	/// Depth of the used tree, or zero for an empty tree.
	pub depth: u16,
	/// Root of the tree, when it has records.
	pub root: Option<NodeId>,
	/// Count of leaf records declared by the tree.
	pub leaf_records: u32,
	/// First leaf in the forward chain, if present.
	pub first_leaf: Option<NodeId>,
	/// Last leaf in the forward chain, if present.
	pub last_leaf: Option<NodeId>,
	/// Size of each node in bytes.
	pub node_bytes: u16,
	/// Declared maximum key length in bytes.
	pub max_key_bytes: u16,
	/// Total nodes, including free nodes.
	pub total_nodes: u32,
	/// Free node count according to the header.
	pub free_nodes: u32,
	/// Allocation clump size hint in bytes.
	pub clump_bytes: u32,
	/// Tree-specific type field.
	pub tree_type: u8,
	/// Tree-specific key comparison code. HFSX readers must use this for name policy.
	pub key_compare_type: u8,
	/// Raw tree attribute bits.
	pub attributes: u32,
}

impl TreeHeader {
	/// Read the candidate node size from the 34-byte header node prefix.
	///
	/// # Errors
	/// Returns an error for a truncated prefix or an invalid node kind or size.
	pub fn node_size_hint(bytes: &[u8]) -> Result<u16, Error> {
		if bytes.get(descriptor::KIND) != Some(&descriptor::HEADER_KIND) {
			return Err(Error::InvalidNode);
		}
		let node_bytes = u16_at(bytes, descriptor::BYTES + header_record::NODE_BYTES)?;
		if !valid_node_size(usize::from(node_bytes)) {
			return Err(Error::InvalidNode);
		}
		Ok(node_bytes)
	}

	/// Read and validate the tree header within the owning fork's logical length.
	/// This checks the recorded roots and counts, not the tree's traversal or bitmap.
	///
	/// # Errors
	/// Returns an error for an invalid header, node count, root or leaf range.
	pub fn decode(node: &Node<'_>, fork_bytes: u64) -> Result<Self, Error> {
		if node.kind != Kind::Header {
			return Err(Error::InvalidNode);
		}
		let bytes = node
			.record(header_record::ORDINAL)
			.filter(|record| record.len() == HEADER_RECORD_BYTES)
			.ok_or(Error::InvalidNode)?;
		let header = Self {
			depth: u16_at(bytes, header_record::DEPTH)?,
			root: optional_id(u32_at(bytes, header_record::ROOT_NODE)?),
			leaf_records: u32_at(bytes, header_record::LEAF_RECORD_COUNT)?,
			first_leaf: optional_id(u32_at(bytes, header_record::FIRST_LEAF)?),
			last_leaf: optional_id(u32_at(bytes, header_record::LAST_LEAF)?),
			node_bytes: u16_at(bytes, header_record::NODE_BYTES)?,
			max_key_bytes: u16_at(bytes, header_record::MAX_KEY_BYTES)?,
			total_nodes: u32_at(bytes, header_record::TOTAL_NODES)?,
			free_nodes: u32_at(bytes, header_record::FREE_NODES)?,
			clump_bytes: u32_at(bytes, header_record::CLUMP_BYTES)?,
			tree_type: bytes[header_record::TREE_TYPE],
			key_compare_type: bytes[header_record::KEY_COMPARE_TYPE],
			attributes: u32_at(bytes, header_record::ATTRIBUTES)?,
		};
		if usize::from(header.node_bytes) != node.bytes.len()
			|| u64::from(header.node_bytes) * u64::from(header.total_nodes) > fork_bytes
			|| node.forward.0 >= header.total_nodes
		{
			return Err(Error::InvalidNode);
		}
		header.validate()?;
		Ok(header)
	}

	fn validate(&self) -> Result<(), Error> {
		let node_ids = [self.root, self.first_leaf, self.last_leaf];
		let layout_matches_depth = if self.depth == EMPTY_TREE_DEPTH {
			node_ids.iter().all(Option::is_none) && self.leaf_records == 0
		} else {
			node_ids.iter().all(Option::is_some) && self.leaf_records != 0
		};
		if !valid_node_size(usize::from(self.node_bytes))
			|| u32::from(self.max_key_bytes) * u32::from(MIN_KEYS_PER_NODE)
				+ descriptor::BYTES as u32
				>= u32::from(self.node_bytes)
			|| self.free_nodes >= self.total_nodes
			|| node_ids
				.into_iter()
				.flatten()
				.any(|id| id.0 == NULL_NODE_ID || id.0 >= self.total_nodes)
			|| self.depth > u16::from(u8::MAX)
			|| !layout_matches_depth
		{
			return Err(Error::InvalidNode);
		}
		Ok(())
	}
}

fn optional_id(id: u32) -> Option<NodeId> {
	(id != NULL_NODE_ID).then_some(NodeId(id))
}

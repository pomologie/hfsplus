//! Fresh node and header layouts follow [Apple TN1150](https://developer.apple.com/library/archive/technotes/tn/tn1150.html).

use super::{
	HEADER_RECORD_BYTES, HEADER_USER_DATA_BYTES, Kind, MIN_KEYS_PER_NODE, NULL_NODE_ID, Node,
	NodeId, RECORD_ALIGNMENT_BYTES, TreeHeader, descriptor, header_record, offset_table_start,
	record_offset_position, valid_descriptor, valid_node_size,
};
use crate::{Error, binary::put};

const MAP_PADDING_BYTES: usize = 2;

/// Forward and backward links stored in a node descriptor.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NodeLinks {
	/// Next node of the same level, or the first map continuation for a header node.
	pub forward: Option<NodeId>,
	/// Previous node of the same level. Header and map nodes leave this empty.
	pub backward: Option<NodeId>,
}

/// Fixed geometry of a B-tree fork.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeLayout {
	/// Size of each node in bytes.
	pub node_bytes: u16,
	/// Maximum key length, excluding its length field.
	pub max_key_bytes: u16,
	/// Number of nodes in the fork, including the header and free nodes.
	pub total_nodes: u32,
	/// Count of free nodes represented in the allocation map.
	pub free_nodes: u32,
}

/// Root and leaf-chain information. The default describes an empty tree.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TreeTopology {
	/// Height of the root or zero for an empty tree.
	pub depth: u16,
	/// Root node identifier.
	pub root: Option<NodeId>,
	/// Count of all records in the leaf chain.
	pub leaf_records: u32,
	/// Identifier of the first leaf node.
	pub first_leaf: Option<NodeId>,
	/// Identifier of the last leaf node.
	pub last_leaf: Option<NodeId>,
}

/// Tree-specific header fields.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TreeMetadata {
	/// Allocation clump size hint in bytes.
	pub clump_bytes: u32,
	/// Tree type code.
	pub tree_type: u8,
	/// Key comparison code associated with the chosen catalog profile.
	pub key_compare_type: u8,
	/// Raw attribute bits. HFS+ uses [`super::BIG_KEYS_MASK`] for two-byte key lengths.
	pub attributes: u32,
}

impl Node<'_> {
	/// Return the total bytes available for records of this count.
	///
	/// # Errors
	/// Returns an error if the node size is invalid or its offset table cannot fit.
	pub fn record_capacity(node_bytes: usize, record_count: usize) -> Result<usize, Error> {
		if !valid_node_size(node_bytes) {
			return Err(Error::InvalidNode);
		}
		Ok(offset_table_start(node_bytes, record_count)? - descriptor::BYTES)
	}

	/// Return the exact size of the allocation bitmap record in a header node.
	///
	/// # Errors
	/// Returns an error if the node size is invalid.
	pub fn header_map_bytes(node_bytes: usize) -> Result<usize, Error> {
		Self::record_capacity(node_bytes, descriptor::HEADER_RECORD_COUNT)?
			.checked_sub(HEADER_RECORD_BYTES + HEADER_USER_DATA_BYTES)
			.ok_or(Error::InvalidNode)
	}

	/// Return the exact size of the allocation bitmap record in a continuation node.
	///
	/// # Errors
	/// Returns an error if the node size is invalid.
	pub fn continuation_map_bytes(node_bytes: usize) -> Result<usize, Error> {
		Self::record_capacity(node_bytes, descriptor::MAP_RECORD_COUNT)?
			.checked_sub(MAP_PADDING_BYTES)
			.ok_or(Error::InvalidNode)
	}

	/// Encode one new node into a complete node-sized destination.
	///
	/// Supply already encoded records in tree order, including any alignment
	/// padding within a record. The owning tree determines key ordering, link
	/// targets and allocation bits. Reserved and unused bytes are zeroed.
	///
	/// # Errors
	/// Rejects an invalid size, descriptor, record width or total occupied size
	/// before writing to the destination.
	pub fn encode(
		destination: &mut [u8],
		kind: Kind,
		height: u8,
		links: NodeLinks,
		records: &[&[u8]],
	) -> Result<(), Error> {
		let node_bytes = destination.len();
		let mut capacity = Self::record_capacity(node_bytes, records.len())?;
		let forward = link_number(links.forward)?;
		let backward = link_number(links.backward)?;
		if !valid_descriptor(kind, height, records.len(), NodeId(backward)) {
			return Err(Error::InvalidNode);
		}
		validate_record_shape(node_bytes, kind, backward, records)?;
		for record in records {
			if record.is_empty() || !record.len().is_multiple_of(RECORD_ALIGNMENT_BYTES) {
				return Err(Error::InvalidNode);
			}
			capacity = capacity
				.checked_sub(record.len())
				.ok_or(Error::InvalidNode)?;
		}
		let count = u16::try_from(records.len()).map_err(|_| Error::InvalidNode)?;

		destination.fill(0);
		put(destination, descriptor::FORWARD_LINK, forward.to_be_bytes())?;
		put(
			destination,
			descriptor::BACKWARD_LINK,
			backward.to_be_bytes(),
		)?;
		put(destination, descriptor::KIND, [kind as u8])?;
		put(destination, descriptor::HEIGHT, [height])?;
		put(destination, descriptor::RECORD_COUNT, count.to_be_bytes())?;

		let mut offset = descriptor::BYTES;
		for (ordinal, record) in records.iter().enumerate() {
			put_record_offset(destination, ordinal, offset)?;
			// Capacity validation guarantees the record ends before the offset table.
			let end = offset + record.len();
			destination[offset..end].copy_from_slice(record);
			offset = end;
		}
		put_record_offset(destination, records.len(), offset)
	}
}

fn validate_record_shape(
	node_bytes: usize,
	kind: Kind,
	backward: u32,
	records: &[&[u8]],
) -> Result<(), Error> {
	match (kind, records) {
		(Kind::Leaf, _) => Ok(()),
		(Kind::Index, _) if records.len() >= usize::from(MIN_KEYS_PER_NODE) => Ok(()),
		(Kind::Header, [header, user_data, map])
			if header.len() == HEADER_RECORD_BYTES
				&& user_data.len() == HEADER_USER_DATA_BYTES
				&& map.len() == Node::header_map_bytes(node_bytes)? =>
		{
			Ok(())
		}
		(Kind::Map, [map])
			if backward == NULL_NODE_ID
				&& map.len() == Node::continuation_map_bytes(node_bytes)? =>
		{
			Ok(())
		}
		_ => Err(Error::InvalidNode),
	}
}

fn put_record_offset(destination: &mut [u8], ordinal: usize, offset: usize) -> Result<(), Error> {
	let offset = u16::try_from(offset).map_err(|_| Error::InvalidNode)?;
	let position = record_offset_position(destination.len(), ordinal);
	put(destination, position, offset.to_be_bytes())
}

fn link_number(id: Option<NodeId>) -> Result<u32, Error> {
	match id {
		None => Ok(NULL_NODE_ID),
		Some(NodeId(NULL_NODE_ID)) => Err(Error::InvalidNode),
		Some(NodeId(id)) => Ok(id),
	}
}

impl TreeHeader {
	/// Construct the header for a new tree using its layout, topology and metadata.
	///
	/// # Errors
	/// Returns an error if node sizes, counts, depth or root and leaf identifiers are inconsistent.
	pub fn new(
		layout: TreeLayout,
		topology: TreeTopology,
		metadata: TreeMetadata,
	) -> Result<Self, Error> {
		let header = Self {
			depth: topology.depth,
			root: topology.root,
			leaf_records: topology.leaf_records,
			first_leaf: topology.first_leaf,
			last_leaf: topology.last_leaf,
			node_bytes: layout.node_bytes,
			max_key_bytes: layout.max_key_bytes,
			total_nodes: layout.total_nodes,
			free_nodes: layout.free_nodes,
			clump_bytes: metadata.clump_bytes,
			tree_type: metadata.tree_type,
			key_compare_type: metadata.key_compare_type,
			attributes: metadata.attributes,
		};
		header.validate()?;
		Ok(header)
	}

	/// Encode a fresh header record, with reserved fields zeroed.
	///
	/// # Errors
	/// Returns an error if fields no longer describe a valid header.
	pub fn encode_record(&self) -> Result<[u8; HEADER_RECORD_BYTES], Error> {
		self.validate()?;
		let mut record = [0; HEADER_RECORD_BYTES];
		put(&mut record, header_record::DEPTH, self.depth.to_be_bytes())?;
		put(
			&mut record,
			header_record::ROOT_NODE,
			link_number(self.root)?.to_be_bytes(),
		)?;
		put(
			&mut record,
			header_record::LEAF_RECORD_COUNT,
			self.leaf_records.to_be_bytes(),
		)?;
		put(
			&mut record,
			header_record::FIRST_LEAF,
			link_number(self.first_leaf)?.to_be_bytes(),
		)?;
		put(
			&mut record,
			header_record::LAST_LEAF,
			link_number(self.last_leaf)?.to_be_bytes(),
		)?;
		put(
			&mut record,
			header_record::NODE_BYTES,
			self.node_bytes.to_be_bytes(),
		)?;
		put(
			&mut record,
			header_record::MAX_KEY_BYTES,
			self.max_key_bytes.to_be_bytes(),
		)?;
		put(
			&mut record,
			header_record::TOTAL_NODES,
			self.total_nodes.to_be_bytes(),
		)?;
		put(
			&mut record,
			header_record::FREE_NODES,
			self.free_nodes.to_be_bytes(),
		)?;
		put(
			&mut record,
			header_record::CLUMP_BYTES,
			self.clump_bytes.to_be_bytes(),
		)?;
		put(&mut record, header_record::TREE_TYPE, [self.tree_type])?;
		put(
			&mut record,
			header_record::KEY_COMPARE_TYPE,
			[self.key_compare_type],
		)?;
		put(
			&mut record,
			header_record::ATTRIBUTES,
			self.attributes.to_be_bytes(),
		)?;
		Ok(record)
	}
}

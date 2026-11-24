//! Deterministic packing of ordered HFS+ keyed records into a new B-tree fork.
//!
//! The owning filesystem operation validates key semantics and order. This
//! layer validates the wire framing, size limits, topology, and node map.

use std::{collections::VecDeque, error::Error, fmt, ops::Range};

use hfsplus_format::btree::{
	BIG_KEYS_MASK, HEADER_USER_DATA_BYTES, Kind, LEAF_HEIGHT, METADATA_NODE_HEIGHT, Node, NodeId,
	NodeLinks, TreeHeader, TreeLayout, TreeMetadata, TreeTopology, VARIABLE_INDEX_KEYS_MASK,
};

const KEY_LENGTH_BYTES: usize = size_of::<u16>();
const RECORD_ALIGNMENT_BYTES: usize = 2;
const RECORD_OFFSET_BYTES: usize = size_of::<u16>();
const CHILD_POINTER_BYTES: usize = size_of::<u32>();
const BITS_PER_MAP_BYTE: usize = u8::BITS as usize;
const FIRST_USED_NODE: usize = 1;
const MIN_INDEX_CHILDREN: usize = 2;

/// A complete encoded leaf record and the width of its encoded key.
#[derive(Clone, Copy, Debug)]
pub struct Record<'a> {
	bytes: &'a [u8],
	encoded_key_len: usize,
}

impl<'a> Record<'a> {
	/// Supplies an already encoded leaf record.
	///
	/// The encoded key length includes the two-byte key-length field and
	/// excludes the optional alignment byte before the value.
	#[must_use]
	pub const fn new(bytes: &'a [u8], encoded_key_len: usize) -> Self {
		Self {
			bytes,
			encoded_key_len,
		}
	}

	fn key(self, max_key_bytes: u16) -> Option<&'a [u8]> {
		let key_len = u16::from_be_bytes(*self.bytes.first_chunk()?);
		let encoded_key_len = usize::from(key_len) + KEY_LENGTH_BYTES;
		if key_len == 0
			|| key_len > max_key_bytes
			|| encoded_key_len != self.encoded_key_len
			|| self.bytes.len() <= aligned(encoded_key_len)
			|| !self.bytes.len().is_multiple_of(RECORD_ALIGNMENT_BYTES)
		{
			return None;
		}
		self.bytes.get(..encoded_key_len)
	}
}

/// The geometry, budget, and header fields for a fresh HFS+ B-tree.
#[derive(Clone, Copy, Debug)]
pub struct TreeOptions {
	/// Fixed size of each node, in bytes.
	pub node_bytes: u16,
	/// Maximum permitted key length, excluding the key-length field.
	pub max_key_bytes: u16,
	/// Maximum number of nodes, including the header and map continuations.
	pub max_nodes: u32,
	/// Header metadata, including the required HFS+ big-key attribute.
	pub metadata: TreeMetadata,
}

/// Failure to build a complete, bounded B-tree.
#[derive(Debug)]
#[non_exhaustive]
pub enum TreeError {
	/// The geometry cannot hold two maximum-width index entries.
	InvalidGeometry,
	/// The leaf record at this ordinal has an invalid wire layout.
	InvalidRecord(usize),
	/// The leaf record at this ordinal does not fit in a node.
	RecordTooLarge(usize),
	/// The supplied record sizes cannot form an index level with valid fanout.
	IndexFanout,
	/// Node count, tree depth, or encoded size exceeds the supported budget.
	NodeLimit,
	/// Memory could not be reserved for the bounded tree.
	Allocation(std::collections::TryReserveError),
	/// The format codec rejected a generated node or header.
	Format(hfsplus_format::Error),
}

impl fmt::Display for TreeError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::InvalidGeometry => f.write_str("invalid HFS+ B-tree geometry"),
			Self::InvalidRecord(ordinal) => {
				write!(f, "invalid HFS+ leaf record at ordinal {ordinal}")
			}
			Self::RecordTooLarge(ordinal) => {
				write!(f, "HFS+ leaf record {ordinal} exceeds one node")
			}
			Self::IndexFanout => f.write_str(
				"cannot partition HFS+ index level into nodes with at least two children",
			),
			Self::NodeLimit => f.write_str("HFS+ tree exceeds its node or depth limit"),
			Self::Allocation(error) => write!(f, "cannot reserve HFS+ tree storage: {error}"),
			Self::Format(error) => write!(f, "cannot encode HFS+ tree: {error}"),
		}
	}
}

impl Error for TreeError {
	fn source(&self) -> Option<&(dyn Error + 'static)> {
		match self {
			Self::Allocation(error) => Some(error),
			Self::Format(error) => Some(error),
			_ => None,
		}
	}
}

impl From<hfsplus_format::Error> for TreeError {
	fn from(value: hfsplus_format::Error) -> Self {
		Self::Format(value)
	}
}

impl From<std::collections::TryReserveError> for TreeError {
	fn from(value: std::collections::TryReserveError) -> Self {
		Self::Allocation(value)
	}
}

/// The complete metadata bytes of a freshly packed tree with no reserved free nodes.
#[derive(Debug)]
pub struct PackedTree {
	bytes: Vec<u8>,
	header: TreeHeader,
}

impl PackedTree {
	/// Packs records in their supplied, semantically sorted order.
	///
	/// It is the caller's responsibility to apply the tree-specific key
	/// comparator and exclude duplicate keys. An empty slice builds a valid
	/// header-only B-tree. Values are copied into the tree's owned metadata.
	///
	/// # Errors
	/// Returns an error for incompatible geometry, malformed or oversized
	/// records, insufficient index fanout, budget exhaustion, or allocation failure.
	pub fn build(records: &[Record<'_>], options: TreeOptions) -> Result<Self, TreeError> {
		let mut packer = Packer::new(options)?;
		let leaf_records = u32::try_from(records.len()).map_err(|_| TreeError::NodeLimit)?;
		let mut level = packer.leaves(records)?;
		let first_leaf = level.first().map(|child| child.id);
		let last_leaf = level.last().map(|child| child.id);
		let mut height = if level.is_empty() {
			METADATA_NODE_HEIGHT
		} else {
			LEAF_HEIGHT
		};
		while level.len() > 1 {
			height = height.checked_add(1).ok_or(TreeError::NodeLimit)?;
			level = packer.index(&level, height)?;
		}
		packer.finish(TreeTopology {
			depth: height.into(),
			root: level.first().map(|child| child.id),
			leaf_records,
			first_leaf,
			last_leaf,
		})
	}

	/// Borrows the entire node image in on-disk order.
	#[must_use]
	pub fn as_bytes(&self) -> &[u8] {
		&self.bytes
	}

	/// Takes ownership of the entire node image.
	#[must_use]
	pub fn into_bytes(self) -> Box<[u8]> {
		self.bytes.into_boxed_slice()
	}

	/// Borrows the topology and geometry encoded in node zero.
	#[must_use]
	pub fn header(&self) -> &TreeHeader {
		&self.header
	}
}

#[derive(Clone, Copy)]
struct Child<'a> {
	id: NodeId,
	first_key: &'a [u8],
}

struct Packer {
	bytes: Vec<u8>,
	options: TreeOptions,
	node_bytes: usize,
	capacity: usize,
}

impl Packer {
	fn new(options: TreeOptions) -> Result<Self, TreeError> {
		let node_bytes = usize::from(options.node_bytes);
		let maximum_index_record =
			aligned(KEY_LENGTH_BYTES + usize::from(options.max_key_bytes)) + CHILD_POINTER_BYTES;
		if options.metadata.attributes & BIG_KEYS_MASK == 0
			|| options.max_nodes == 0
			|| options.max_key_bytes == 0
			|| maximum_index_record * MIN_INDEX_CHILDREN
				> Node::record_capacity(node_bytes, MIN_INDEX_CHILDREN)?
		{
			return Err(TreeError::InvalidGeometry);
		}
		let mut result = Self {
			bytes: Vec::new(),
			options,
			node_bytes,
			capacity: Node::record_capacity(node_bytes, 0)?,
		};
		result.append_node()?;
		Ok(result)
	}

	fn leaves<'a>(&mut self, records: &[Record<'a>]) -> Result<Vec<Child<'a>>, TreeError> {
		let mut groups = Vec::new();
		let mut start = 0;
		let mut occupied = 0;
		for (ordinal, record) in records.iter().enumerate() {
			record
				.key(self.options.max_key_bytes)
				.ok_or(TreeError::InvalidRecord(ordinal))?;
			let space = record
				.bytes
				.len()
				.checked_add(RECORD_OFFSET_BYTES)
				.filter(|space| *space <= self.capacity)
				.ok_or(TreeError::RecordTooLarge(ordinal))?;
			if occupied + space > self.capacity {
				self.add_leaf_group(&mut groups, start..ordinal)?;
				start = ordinal;
				occupied = 0;
			}
			occupied += space;
		}
		if start < records.len() {
			self.add_leaf_group(&mut groups, start..records.len())?;
		}
		let mut level = Vec::new();
		level.try_reserve_exact(groups.len())?;
		let mut slices = Vec::new();
		for (ordinal, range) in groups.iter().enumerate() {
			let group = &records[range.clone()];
			slices.clear();
			slices.try_reserve(group.len())?;
			slices.extend(group.iter().map(|record| record.bytes));
			let node = self.node_count();
			let links = sibling_links(node, ordinal, groups.len())?;
			let id = node_id(node)?;
			let key = group[0]
				.key(self.options.max_key_bytes)
				.ok_or(TreeError::InvalidRecord(range.start))?;
			Node::encode(self.append_node()?, Kind::Leaf, LEAF_HEIGHT, links, &slices)?;
			level.push(Child { id, first_key: key });
		}
		Ok(level)
	}

	fn add_leaf_group(
		&self,
		groups: &mut Vec<Range<usize>>,
		range: Range<usize>,
	) -> Result<(), TreeError> {
		if groups.len() + FIRST_USED_NODE >= self.options.max_nodes as usize {
			return Err(TreeError::NodeLimit);
		}
		groups.try_reserve(1)?;
		groups.push(range);
		Ok(())
	}

	fn index<'a>(
		&mut self,
		children: &[Child<'a>],
		height: u8,
	) -> Result<Vec<Child<'a>>, TreeError> {
		let fixed_slot_bytes = (self.options.metadata.attributes & VARIABLE_INDEX_KEYS_MASK == 0)
			.then_some(KEY_LENGTH_BYTES + usize::from(self.options.max_key_bytes));
		let record_size = |child: &Child<'_>| {
			aligned(fixed_slot_bytes.unwrap_or(child.first_key.len())) + CHILD_POINTER_BYTES
		};
		let groups = index_groups(
			children
				.iter()
				.map(|child| record_size(child) + RECORD_OFFSET_BYTES),
			self.capacity,
		)?;
		let mut parents = Vec::new();
		parents.try_reserve_exact(groups.len())?;
		let mut staging = Vec::new();
		staging.try_reserve_exact(self.capacity)?;
		for (ordinal, range) in groups.iter().enumerate() {
			let group = &children[range.clone()];
			staging.clear();
			for child in group {
				staging.extend_from_slice(child.first_key);
				let key_slot = record_size(child) - CHILD_POINTER_BYTES;
				staging.resize(staging.len() + key_slot - child.first_key.len(), 0);
				staging.extend_from_slice(&child.id.0.to_be_bytes());
			}
			let mut slices = Vec::new();
			slices.try_reserve_exact(group.len())?;
			let mut start = 0;
			for child in group {
				let end = start + record_size(child);
				slices.push(&staging[start..end]);
				start = end;
			}
			let node = self.node_count();
			let id = node_id(node)?;
			let links = sibling_links(node, ordinal, groups.len())?;
			Node::encode(self.append_node()?, Kind::Index, height, links, &slices)?;
			parents.push(Child {
				id,
				first_key: group[0].first_key,
			});
		}
		Ok(parents)
	}

	fn finish(mut self, topology: TreeTopology) -> Result<PackedTree, TreeError> {
		let header_map_bytes = Node::header_map_bytes(self.node_bytes)?;
		let map_node_bytes = Node::continuation_map_bytes(self.node_bytes)?;
		let map_start = self.node_count();
		// Each continuation adds its bitmap capacity and consumes one bit for itself.
		let map_nodes = map_start
			.saturating_sub(header_map_bytes * BITS_PER_MAP_BYTE)
			.div_ceil(map_node_bytes * BITS_PER_MAP_BYTE - 1);
		let total = map_start
			.checked_add(map_nodes)
			.filter(|total| *total <= self.options.max_nodes as usize)
			.ok_or(TreeError::NodeLimit)?;
		let header = TreeHeader::new(
			TreeLayout {
				node_bytes: self.options.node_bytes,
				max_key_bytes: self.options.max_key_bytes,
				total_nodes: node_id(total)?.0,
				free_nodes: 0,
			},
			topology,
			self.options.metadata,
		)?;
		let first_map_id = (map_nodes > 0).then(|| node_id(map_start)).transpose()?;
		let mut map = Vec::new();
		map.try_reserve_exact(header_map_bytes.max(map_node_bytes))?;
		map.resize(header_map_bytes, 0);
		mark_allocated_prefix(&mut map, total);
		Node::encode(
			&mut self.bytes[..self.node_bytes],
			Kind::Header,
			METADATA_NODE_HEIGHT,
			NodeLinks {
				forward: first_map_id,
				backward: None,
			},
			&[&header.encode_record()?, &[0; HEADER_USER_DATA_BYTES], &map],
		)?;
		map.resize(map_node_bytes, 0);
		let mut remaining = total.saturating_sub(header_map_bytes * BITS_PER_MAP_BYTE);
		for ordinal in 0..map_nodes {
			mark_allocated_prefix(&mut map, remaining);
			let next = (ordinal + 1 < map_nodes)
				.then(|| node_id(map_start + ordinal + 1))
				.transpose()?;
			Node::encode(
				self.append_node()?,
				Kind::Map,
				METADATA_NODE_HEIGHT,
				NodeLinks {
					forward: next,
					backward: None,
				},
				&[&map],
			)?;
			remaining = remaining.saturating_sub(map_node_bytes * BITS_PER_MAP_BYTE);
		}
		Ok(PackedTree {
			bytes: self.bytes,
			header,
		})
	}

	fn node_count(&self) -> usize {
		self.bytes.len() / self.node_bytes
	}

	fn append_node(&mut self) -> Result<&mut [u8], TreeError> {
		if self.node_count() >= self.options.max_nodes as usize {
			return Err(TreeError::NodeLimit);
		}
		let start = self.bytes.len();
		let end = start
			.checked_add(self.node_bytes)
			.ok_or(TreeError::NodeLimit)?;
		self.bytes.try_reserve(self.node_bytes)?;
		self.bytes.resize(end, 0);
		Ok(&mut self.bytes[start..])
	}
}

fn aligned(encoded_key_len: usize) -> usize {
	encoded_key_len.next_multiple_of(RECORD_ALIGNMENT_BYTES)
}

fn node_id(ordinal: usize) -> Result<NodeId, TreeError> {
	u32::try_from(ordinal)
		.map(NodeId)
		.map_err(|_| TreeError::NodeLimit)
}

fn sibling_links(node: usize, ordinal: usize, siblings: usize) -> Result<NodeLinks, TreeError> {
	Ok(NodeLinks {
		forward: (ordinal + 1 < siblings)
			.then(|| node_id(node + 1))
			.transpose()?,
		backward: (ordinal > 0).then(|| node_id(node - 1)).transpose()?,
	})
}

/// Find the fullest feasible group at each point without stranding a singleton.
///
/// Record costs are positive. Walking the possible starts backwards allows
/// both the right capacity bound and the feasible suffix endpoint deque to
/// move monotonically left, independently of varying key widths.
fn index_groups(
	sizes: impl ExactSizeIterator<Item = usize>,
	capacity: usize,
) -> Result<Vec<Range<usize>>, TreeError> {
	let count = sizes.len();
	let slots = count.checked_add(1).ok_or(TreeError::NodeLimit)?;
	let mut prefix: Vec<usize> = Vec::new();
	prefix.try_reserve_exact(slots)?;
	prefix.push(0);
	for size in sizes {
		prefix.push(
			prefix[prefix.len() - 1]
				.checked_add(size)
				.ok_or(TreeError::NodeLimit)?,
		);
	}
	let mut next = Vec::new();
	next.try_reserve_exact(count)?;
	next.resize(count, None);
	let mut reachable = VecDeque::new();
	reachable.try_reserve(slots)?;
	reachable.push_back(count);
	for start in (0..count).rev() {
		while let Some(&end) = reachable.front() {
			if prefix[end] - prefix[start] <= capacity {
				break;
			}
			reachable.pop_front();
		}
		if let Some(end) = reachable
			.front()
			.copied()
			.filter(|end| end - start >= MIN_INDEX_CHILDREN)
		{
			next[start] = Some(end);
			reachable.push_back(start);
		}
	}
	let mut groups = Vec::new();
	let mut start = 0;
	while start < count {
		let end = next[start].ok_or(TreeError::IndexFanout)?;
		groups.try_reserve(1)?;
		groups.push(start..end);
		start = end;
	}
	Ok(groups)
}

fn mark_allocated_prefix(bitmap: &mut [u8], count: usize) {
	let count = count.min(bitmap.len() * BITS_PER_MAP_BYTE);
	let full_bytes = count / BITS_PER_MAP_BYTE;
	let tail = count % BITS_PER_MAP_BYTE;
	bitmap.fill(0);
	bitmap[..full_bytes].fill(u8::MAX);
	if tail != 0 {
		bitmap[full_bytes] = u8::MAX << (BITS_PER_MAP_BYTE - tail);
	}
}

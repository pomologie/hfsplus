use alloc::vec::Vec;
use core::{cmp::Ordering, mem::size_of, ops::ControlFlow};

use hfsplus_core::{ByteOffset, ReadAt};
use hfsplus_format::{
	Error as FormatError,
	attributes::{Key, MAX_KEY_LENGTH, MAX_NAME_UNITS, NATIVE_KEY_COMPARE_TYPE, NATIVE_NODE_BYTES},
	btree::{
		BIG_KEYS_MASK, HFS_BTREE_TYPE, Kind, LEAF_HEIGHT, MIN_INDEX_HEIGHT, Node, NodeId,
		TreeHeader, VARIABLE_INDEX_KEYS_MASK,
	},
	volume::CatalogId,
};

use crate::ForkReader;

use super::{AttributeReadError as Error, FIRST_ATTRIBUTE_BLOCK};

const HEADER_NODE: NodeId = NodeId(0);
const HEADER_PREFIX_BYTES: usize = 34;
const HEADER_MAP_ORDINAL: usize = 2;
const BITS_PER_MAP_BYTE: u32 = u8::BITS;
const FIRST_MAP_BIT: u8 = 1 << (u8::BITS - 1);
const INDEX_CHILD_BYTES: usize = size_of::<u32>();
const NATIVE_TREE_ATTRIBUTES: u32 = BIG_KEYS_MASK | VARIABLE_INDEX_KEYS_MASK;

#[derive(Clone, Copy)]
pub(super) struct Query<'name> {
	pub(super) owner: CatalogId,
	pub(super) name: &'name [u16],
}

impl Query<'_> {
	fn compare(self, other: Key<'_>) -> Ordering {
		compare(self.owner, self.name, FIRST_ATTRIBUTE_BLOCK, other)
	}
}

#[derive(Debug)]
pub(super) struct Tree<'volume, S> {
	fork: ForkReader<'volume, S>,
	header: TreeHeader,
	allocated: Vec<u8>,
}

impl<'volume, S: ReadAt> Tree<'volume, S> {
	pub(super) fn open(fork: ForkReader<'volume, S>) -> Result<Self, Error<S::Error>> {
		let mut prefix = [0; HEADER_PREFIX_BYTES];
		fork.read_exact_at(ByteOffset(0), &mut prefix)
			.map_err(Error::Fork)?;
		let node_bytes = TreeHeader::node_size_hint(&prefix)
			.map_err(|error| Error::format(HEADER_NODE, error))?;
		if node_bytes != NATIVE_NODE_BYTES {
			return Err(Error::UnsupportedTree);
		}
		let mut buffer = new_buffer()?;
		fork.read_exact_at(ByteOffset(0), &mut buffer)
			.map_err(Error::Fork)?;
		let node = Node::decode(&buffer).map_err(|error| Error::format(HEADER_NODE, error))?;
		let header = TreeHeader::decode(&node, fork.len().0)
			.map_err(|error| Error::format(HEADER_NODE, error))?;
		if header.tree_type != HFS_BTREE_TYPE
			|| header.key_compare_type != NATIVE_KEY_COMPARE_TYPE
			|| header.attributes != NATIVE_TREE_ATTRIBUTES
			|| usize::from(header.max_key_bytes) != MAX_KEY_LENGTH
			|| node.forward().is_some()
		{
			return Err(Error::UnsupportedTree);
		}
		let map_len = header.total_nodes.div_ceil(BITS_PER_MAP_BYTE) as usize;
		let map = node
			.record(HEADER_MAP_ORDINAL)
			.ok_or_else(|| invalid_node(HEADER_NODE))?;
		let map = map.get(..map_len).ok_or(Error::UnsupportedTree)?;
		let allocated_count = (0..header.total_nodes)
			.filter(|&index| is_allocated(map, NodeId(index)))
			.count() as u32;
		if header.total_nodes - allocated_count != header.free_nodes
			|| [
				Some(HEADER_NODE),
				header.root,
				header.first_leaf,
				header.last_leaf,
			]
			.into_iter()
			.flatten()
			.any(|id| !is_allocated(map, id))
		{
			return Err(invalid_node(HEADER_NODE));
		}
		let mut allocated = Vec::new();
		allocated
			.try_reserve_exact(map.len())
			.map_err(Error::Allocation)?;
		allocated.extend_from_slice(map);
		Ok(Self {
			fork,
			header,
			allocated,
		})
	}

	pub(super) fn scan<E: From<Error<S::Error>>>(
		&self,
		query: Query<'_>,
		max_nodes: usize,
		mut visit: impl FnMut(NodeId, Key<'_>, &[u8]) -> Result<ControlFlow<()>, E>,
	) -> Result<(), E> {
		let Some(root) = self.header.root else {
			return Ok(());
		};
		if usize::from(self.header.depth) > max_nodes {
			return Err(Error::NodeLimit.into());
		}
		let mut buffer = new_buffer::<S::Error>()?;
		let mut remaining = max_nodes;
		let (mut id, mut bounds) = self.locate(root, query, &mut remaining, &mut buffer)?;
		let mut previous: Option<(NodeId, OwnedKey)> = None;
		loop {
			let leaf = self.decode_node(id, &buffer, Kind::Leaf, u16::from(LEAF_HEIGHT))?;
			let keys = validate_keys::<S::Error>(id, &leaf, &bounds)?;
			if let Some((prev_id, last_key)) = &previous
				&& (leaf.backward() != Some(*prev_id) || last_key.compare(keys.0) != Ordering::Less)
			{
				return Err(invalid_node::<S::Error>(id).into());
			}
			for raw in leaf.records() {
				let (key, _) = decode_key::<S::Error>(id, raw)?;
				if query.compare(key) != Ordering::Greater && visit(id, key, raw)?.is_break() {
					return Ok(());
				}
			}
			let Some(next) = leaf.forward() else {
				return Ok(());
			};
			previous = Some((id, OwnedKey::new(keys.1)));
			bounds = Bounds {
				lower: bounds.upper.take(),
				upper: None,
			};
			id = next;
			self.read_node(id, &mut remaining, &mut buffer)?;
		}
	}

	fn locate(
		&self,
		mut id: NodeId,
		query: Query<'_>,
		remaining: &mut usize,
		buffer: &mut [u8],
	) -> Result<(NodeId, Bounds), Error<S::Error>> {
		let mut bounds = Bounds::default();
		for height in (u16::from(MIN_INDEX_HEIGHT)..=self.header.depth).rev() {
			self.read_node(id, remaining, buffer)?;
			let index = self.decode_node(id, buffer, Kind::Index, height)?;
			validate_keys(id, &index, &bounds)?;
			let mut chosen = 0;
			for (ordinal, raw) in index.records().enumerate() {
				let (key, child) = decode_key(id, raw)?;
				self.index_child(id, child)?;
				if query.compare(key) != Ordering::Less {
					chosen = ordinal;
				}
			}
			let raw = index.record(chosen).ok_or_else(|| invalid_node(id))?;
			let (lower, child) = decode_key(id, raw)?;
			bounds.lower = Some(OwnedKey::new(lower));
			if let Some(raw) = index.record(chosen + 1) {
				bounds.upper = Some(OwnedKey::new(decode_key(id, raw)?.0));
			}
			id = self.index_child(id, child)?;
		}
		self.read_node(id, remaining, buffer)?;
		Ok((id, bounds))
	}

	fn index_child(&self, parent: NodeId, raw: &[u8]) -> Result<NodeId, Error<S::Error>> {
		let bytes: [u8; INDEX_CHILD_BYTES] = raw.try_into().map_err(|_| invalid_node(parent))?;
		let id = NodeId(u32::from_be_bytes(bytes));
		if id == parent || !self.valid_id(id) {
			return Err(invalid_node(parent));
		}
		Ok(id)
	}

	fn read_node(
		&self,
		id: NodeId,
		remaining: &mut usize,
		buffer: &mut [u8],
	) -> Result<(), Error<S::Error>> {
		if !self.valid_id(id) {
			return Err(invalid_node(id));
		}
		*remaining = remaining.checked_sub(1).ok_or(Error::NodeLimit)?;
		let offset = ByteOffset(u64::from(id.0) * u64::from(self.header.node_bytes));
		self.fork.read_exact_at(offset, buffer).map_err(Error::Fork)
	}

	fn decode_node<'bytes>(
		&self,
		id: NodeId,
		buffer: &'bytes [u8],
		kind: Kind,
		height: u16,
	) -> Result<Node<'bytes>, Error<S::Error>> {
		let node = Node::decode(buffer).map_err(|error| Error::format(id, error))?;
		if node.kind() != kind
			|| u16::from(node.height()) != height
			|| [node.backward(), node.forward()]
				.into_iter()
				.flatten()
				.any(|linked| linked == id || !self.valid_id(linked))
			|| (Some(id) == self.header.root
				&& (node.backward().is_some() || node.forward().is_some()))
			|| (kind == Kind::Leaf
				&& ((Some(id) == self.header.first_leaf) != node.backward().is_none()
					|| (Some(id) == self.header.last_leaf) != node.forward().is_none()))
		{
			return Err(invalid_node(id));
		}
		Ok(node)
	}

	fn valid_id(&self, id: NodeId) -> bool {
		id != HEADER_NODE && id.0 < self.header.total_nodes && is_allocated(&self.allocated, id)
	}
}

#[derive(Default)]
struct Bounds {
	lower: Option<OwnedKey>,
	upper: Option<OwnedKey>,
}

struct OwnedKey {
	owner: CatalogId,
	units: [u16; MAX_NAME_UNITS],
	len: usize,
	start_block: u32,
}

impl OwnedKey {
	fn new(key: Key<'_>) -> Self {
		let decoded = key.name.units();
		let mut units = [0; MAX_NAME_UNITS];
		let len = decoded.len();
		for (target, unit) in units.iter_mut().zip(decoded) {
			*target = unit;
		}
		Self {
			owner: key.file_id,
			units,
			len,
			start_block: key.start_block,
		}
	}

	fn compare(&self, other: Key<'_>) -> Ordering {
		compare(self.owner, &self.units[..self.len], self.start_block, other)
	}
}

fn compare(owner: CatalogId, name: &[u16], start_block: u32, other: Key<'_>) -> Ordering {
	owner
		.cmp(&other.file_id)
		.then_with(|| name.iter().copied().cmp(other.name.units()))
		.then_with(|| start_block.cmp(&other.start_block))
}

fn validate_keys<'bytes, E>(
	id: NodeId,
	node: &Node<'bytes>,
	bounds: &Bounds,
) -> Result<(Key<'bytes>, Key<'bytes>), Error<E>> {
	let mut records = node.records();
	let raw = records.next().ok_or_else(|| invalid_node(id))?;
	let (first, _) = decode_key(id, raw)?;
	if bounds
		.lower
		.as_ref()
		.is_some_and(|min| min.compare(first) != Ordering::Equal)
	{
		return Err(invalid_node(id));
	}
	let mut last = first;
	for raw in records {
		let (key, _) = decode_key(id, raw)?;
		if last >= key {
			return Err(invalid_node(id));
		}
		last = key;
	}
	if bounds
		.upper
		.as_ref()
		.is_some_and(|max| max.compare(last) != Ordering::Greater)
	{
		return Err(invalid_node(id));
	}
	Ok((first, last))
}

fn decode_key<E>(id: NodeId, raw: &[u8]) -> Result<(Key<'_>, &[u8]), Error<E>> {
	Key::split(raw).map_err(|error| Error::format(id, error))
}

fn is_allocated(map: &[u8], id: NodeId) -> bool {
	let byte = (id.0 / BITS_PER_MAP_BYTE) as usize;
	let mask = FIRST_MAP_BIT >> (id.0 % BITS_PER_MAP_BYTE);
	map.get(byte).is_some_and(|value| value & mask != 0)
}

fn invalid_node<E>(id: NodeId) -> Error<E> {
	Error::format(id, FormatError::InvalidNode)
}

fn new_buffer<E>() -> Result<Vec<u8>, Error<E>> {
	let node_bytes = usize::from(NATIVE_NODE_BYTES);
	let mut buffer = Vec::new();
	buffer
		.try_reserve_exact(node_bytes)
		.map_err(Error::Allocation)?;
	buffer.resize(node_bytes, 0);
	Ok(buffer)
}

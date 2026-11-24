use alloc::vec::Vec;
use core::cmp::Ordering;

use hfsplus_core::{
	ByteOffset, ReadAt,
	names::{MAX_NAME_UNITS, NameCollation},
};
use hfsplus_format::{
	btree::{
		BIG_KEYS_MASK, HEADER_USER_DATA_BYTES, HFS_BTREE_TYPE, Kind, LEAF_HEIGHT, Node, NodeId,
		TreeHeader, VARIABLE_INDEX_KEYS_MASK,
	},
	catalog::{Key, MAX_KEY_LENGTH},
	volume::CatalogId,
};

use super::{CatalogError, CatalogLimits, Corruption, Limit, format, invalid};
use crate::Error;

pub(super) const HEADER_ID: NodeId = NodeId(0);
const HEADER_PREFIX_BYTES: usize = 34;
const CATALOG_MIN_NODE_BYTES: u16 = 4096;
const HEADER_USER_RECORD: usize = 1;
const HEADER_MAP_RECORD: usize = 2;
const CONTINUATION_MAP_RECORD: usize = 0;
const BITS_PER_BYTE: usize = u8::BITS as usize;
const HIGH_MAP_BIT: u8 = 1 << (u8::BITS - 1);
const CATALOG_ATTRIBUTES: u32 = BIG_KEYS_MASK | VARIABLE_INDEX_KEYS_MASK;
const FIRST_ORDINAL: usize = 0;
const MIN_INDEX_CHILDREN: usize = 2;
const COLLATION: NameCollation = NameCollation::CaseFolded;

#[derive(Debug)]
pub(super) struct Tree<R> {
	fork: R,
	header: TreeHeader,
	map: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Position {
	pub(super) node: NodeId,
	pub(super) ordinal: usize,
}

pub(super) struct Query {
	buffer: Vec<u8>,
	remaining: u32,
}

impl Query {
	pub(super) fn new<E>(node_bytes: usize, remaining: u32) -> Result<Self, CatalogError<E>> {
		Ok(Self {
			buffer: node_buffer(node_bytes)?,
			remaining,
		})
	}

	pub(super) fn record<E>(&self, position: Position) -> Result<&[u8], CatalogError<E>> {
		let node = Node::decode(&self.buffer).map_err(|error| format(position.node, error))?;
		node.record(position.ordinal)
			.ok_or(invalid(position.node, Corruption::RecordOrder))
	}

	fn read<R, E>(&mut self, fork: &R, id: NodeId) -> Result<Node<'_>, CatalogError<E>>
	where
		R: ReadAt<Error = Error<E>>,
	{
		read_fork_node(fork, id, &mut self.buffer, &mut self.remaining)
	}
}

pub(super) fn node_buffer<E>(node_bytes: usize) -> Result<Vec<u8>, CatalogError<E>> {
	let mut buffer = Vec::new();
	buffer
		.try_reserve_exact(node_bytes)
		.map_err(CatalogError::Allocation)?;
	buffer.resize(node_bytes, 0);
	Ok(buffer)
}

fn read_fork_node<'buffer, R, E>(
	fork: &R,
	id: NodeId,
	buffer: &'buffer mut [u8],
	remaining: &mut u32,
) -> Result<Node<'buffer>, CatalogError<E>>
where
	R: ReadAt<Error = Error<E>>,
{
	*remaining = remaining
		.checked_sub(1)
		.ok_or(CatalogError::Limit(Limit::Nodes))?;
	let offset = u64::from(id.0) * buffer.len() as u64;
	fork.read_exact_at(ByteOffset(offset), buffer)
		.map_err(CatalogError::Read)?;
	Node::decode(buffer).map_err(|error| format(id, error))
}

impl<R, E> Tree<R>
where
	R: ReadAt<Error = Error<E>>,
{
	pub(super) fn open(fork: R, limits: CatalogLimits) -> Result<Self, CatalogError<E>> {
		let mut prefix = [0; HEADER_PREFIX_BYTES];
		fork.read_exact_at(ByteOffset(0), &mut prefix)
			.map_err(CatalogError::Read)?;
		let node_size =
			TreeHeader::node_size_hint(&prefix).map_err(|error| format(HEADER_ID, error))?;
		if node_size < CATALOG_MIN_NODE_BYTES {
			return Err(CatalogError::UnsupportedTree);
		}
		let mut query = Query::new(usize::from(node_size), limits.max_nodes_per_query)?;
		let node = query.read(&fork, HEADER_ID)?;
		let header =
			TreeHeader::decode(&node, fork.len().0).map_err(|error| format(HEADER_ID, error))?;
		if header.tree_type != HFS_BTREE_TYPE
			|| header.attributes != CATALOG_ATTRIBUTES
			|| usize::from(header.max_key_bytes) != MAX_KEY_LENGTH
		{
			return Err(CatalogError::UnsupportedTree);
		}
		if header.depth > u16::from(limits.max_depth) {
			return Err(CatalogError::Limit(Limit::Depth));
		}
		let Some(root) = header.root else {
			return Err(invalid(HEADER_ID, Corruption::MissingRoot));
		};
		if header.depth == u16::from(LEAF_HEIGHT)
			&& (header.first_leaf != Some(root) || header.last_leaf != Some(root))
		{
			return Err(invalid(HEADER_ID, Corruption::NodeLinks));
		}
		let map_len = usize::try_from(header.total_nodes.div_ceil(u8::BITS))
			.ok()
			.filter(|&bytes| bytes <= limits.max_map_bytes)
			.ok_or(CatalogError::Limit(Limit::AllocationMap))?;
		let user = node
			.record(HEADER_USER_RECORD)
			.ok_or(invalid(HEADER_ID, Corruption::AllocationMap))?;
		let head = node
			.record(HEADER_MAP_RECORD)
			.ok_or(invalid(HEADER_ID, Corruption::AllocationMap))?;
		let node_bytes = usize::from(node_size);
		let header_map_bytes =
			Node::header_map_bytes(node_bytes).map_err(|error| format(HEADER_ID, error))?;
		let continuation_bytes =
			Node::continuation_map_bytes(node_bytes).map_err(|error| format(HEADER_ID, error))?;
		if user.len() != HEADER_USER_DATA_BYTES || head.len() != header_map_bytes {
			return Err(invalid(HEADER_ID, Corruption::AllocationMap));
		}
		let mut next = node.forward();
		let mut map = Vec::new();
		map.try_reserve_exact(map_len)
			.map_err(CatalogError::Allocation)?;
		append_map(&mut map, head, map_len);
		let mut continuations = Vec::new();
		while let Some(id) = next {
			if id.0 >= header.total_nodes || map.len() == map_len || continuations.contains(&id) {
				return Err(invalid(id, Corruption::AllocationMap));
			}
			continuations
				.try_reserve(1)
				.map_err(CatalogError::Allocation)?;
			continuations.push(id);
			let map_node = query.read(&fork, id)?;
			if map_node.kind() != Kind::Map || map_node.backward().is_some() {
				return Err(invalid(id, Corruption::AllocationMap));
			}
			let record = map_node
				.record(CONTINUATION_MAP_RECORD)
				.ok_or(invalid(id, Corruption::AllocationMap))?;
			if record.len() != continuation_bytes {
				return Err(invalid(id, Corruption::AllocationMap));
			}
			append_map(&mut map, record, map_len);
			next = map_node.forward();
		}
		if map.len() != map_len
			|| !allocated(&map, HEADER_ID)
			|| continuations.into_iter().any(|id| !allocated(&map, id))
			|| map_count(&map, header.total_nodes) != header.total_nodes - header.free_nodes
			|| [header.root, header.first_leaf, header.last_leaf]
				.into_iter()
				.flatten()
				.any(|id| !allocated(&map, id))
		{
			return Err(invalid(HEADER_ID, Corruption::AllocationMap));
		}
		Ok(Self { fork, header, map })
	}

	pub(super) fn node_bytes(&self) -> usize {
		usize::from(self.header.node_bytes)
	}

	pub(super) fn find(
		&self,
		id: CatalogId,
		name: &[u16],
		query: &mut Query,
	) -> Result<Option<Position>, CatalogError<E>> {
		let target = SearchKey { id, name };
		let Some(mut current) = self.header.root else {
			return Err(invalid(HEADER_ID, Corruption::MissingRoot));
		};
		let mut height = self.header.depth;
		let mut bounds = KeyBounds::default();
		loop {
			self.check_address(current)?;
			let node = query.read(&self.fork, current)?;
			self.check_node(current, &node)?;
			if u16::from(node.height()) != height {
				return Err(invalid(current, Corruption::NodeKind));
			}
			let result = self.search_node(current, &node, &mut bounds, target)?;
			match result {
				NodeSearch::Absent => return Ok(None),
				NodeSearch::Match(ordinal) => {
					return Ok(Some(Position {
						node: current,
						ordinal,
					}));
				}
				NodeSearch::Descend(child) => {
					current = child;
					height -= 1;
				}
			}
		}
	}

	pub(super) fn read_leaf<'buffer>(
		&self,
		id: NodeId,
		buffer: &'buffer mut [u8],
		query: &mut Query,
	) -> Result<Node<'buffer>, CatalogError<E>> {
		self.check_address(id)?;
		let node = read_fork_node(&self.fork, id, buffer, &mut query.remaining)?;
		self.check_node(id, &node)?;
		if node.kind() != Kind::Leaf {
			return Err(invalid(id, Corruption::NodeKind));
		}
		validate_order(id, &node)?;
		Ok(node)
	}

	fn check_address(&self, id: NodeId) -> Result<(), CatalogError<E>> {
		if !self.admits_node(id) {
			return Err(invalid(id, Corruption::NodeAddress));
		}
		Ok(())
	}

	fn check_node(&self, id: NodeId, node: &Node<'_>) -> Result<(), CatalogError<E>> {
		if !matches!(node.kind(), Kind::Leaf | Kind::Index) {
			return Err(invalid(id, Corruption::NodeKind));
		}
		let no_backward = node.backward().is_none();
		let no_forward = node.forward().is_none();
		if [node.forward(), node.backward()]
			.into_iter()
			.flatten()
			.any(|link| link == id || !self.admits_node(link))
			|| (Some(id) == self.header.root && !(no_backward && no_forward))
			|| (node.kind() == Kind::Leaf
				&& (no_backward != (self.header.first_leaf == Some(id))
					|| no_forward != (self.header.last_leaf == Some(id))))
		{
			return Err(invalid(id, Corruption::NodeLinks));
		}
		Ok(())
	}

	fn admits_node(&self, id: NodeId) -> bool {
		id != HEADER_ID && id.0 < self.header.total_nodes && allocated(&self.map, id)
	}

	fn search_node(
		&self,
		id: NodeId,
		node: &Node<'_>,
		bounds: &mut KeyBounds,
		target: SearchKey<'_>,
	) -> Result<NodeSearch, CatalogError<E>> {
		// TN1150's index record is the first key of its child. The next
		// separator, when present, is an exclusive bound on all child keys.
		let mut prior = None;
		let mut matched = None;
		let mut selected = None;
		let mut upper = None;
		for (ordinal, raw) in node.records().enumerate() {
			let (key, data) = split_key(id, raw)?;
			if prior.is_some_and(|previous| compare_keys(previous, key) != Ordering::Less) {
				return Err(invalid(id, Corruption::RecordOrder));
			}
			if (ordinal == FIRST_ORDINAL
				&& bounds
					.lower
					.as_ref()
					.is_some_and(|lower| lower.compare(key) != Ordering::Equal))
				|| bounds
					.upper
					.as_ref()
					.is_some_and(|upper| upper.compare(key) != Ordering::Greater)
			{
				return Err(invalid(id, Corruption::IndexRoute));
			}
			prior = Some(key);
			let order = target.compare(key);
			if node.kind() == Kind::Index {
				let pointer: [u8; size_of::<u32>()] = data
					.try_into()
					.map_err(|_| invalid(id, Corruption::IndexRoute))?;
				let child = NodeId(u32::from_be_bytes(pointer));
				if child == id || !self.admits_node(child) {
					return Err(invalid(id, Corruption::IndexRoute));
				}
				if order != Ordering::Less {
					selected = Some((child, key));
				} else if upper.is_none() {
					upper = Some(key);
				}
			} else if order == Ordering::Equal {
				matched = Some(ordinal);
			}
		}
		if node.kind() == Kind::Index && node.records().count() < MIN_INDEX_CHILDREN {
			return Err(invalid(id, Corruption::IndexRoute));
		}
		if let Some((child, lower)) = selected {
			bounds.lower = Some(OwnedKey::from_key(lower));
			if let Some(upper) = upper {
				bounds.upper = Some(OwnedKey::from_key(upper));
			}
			return Ok(NodeSearch::Descend(child));
		}
		Ok(matched.map_or(NodeSearch::Absent, NodeSearch::Match))
	}
}

fn append_map(map: &mut Vec<u8>, record: &[u8], map_len: usize) {
	map.extend_from_slice(&record[..record.len().min(map_len - map.len())]);
}

fn allocated(map: &[u8], id: NodeId) -> bool {
	let bit = id.0 as usize;
	map.get(bit / BITS_PER_BYTE)
		.is_some_and(|byte| byte & (HIGH_MAP_BIT >> (bit % BITS_PER_BYTE)) != 0)
}

fn map_count(map: &[u8], nodes: u32) -> u32 {
	let trailing_unused = (u8::BITS - nodes % u8::BITS) % u8::BITS;
	let Some((last, complete)) = map.split_last() else {
		return 0;
	};
	complete.iter().map(|byte| byte.count_ones()).sum::<u32>()
		+ (last >> trailing_unused).count_ones()
}

pub(super) fn split_key<E>(id: NodeId, raw: &[u8]) -> Result<(Key<'_>, &[u8]), CatalogError<E>> {
	let (key, data) = Key::split_leaf(raw).map_err(|error| format(id, error))?;
	if !key.name.is_empty() && collates_empty(OwnedKey::from_key(key).name()) {
		return Err(invalid(id, Corruption::RecordOrder));
	}
	Ok((key, data))
}

fn validate_order<E>(id: NodeId, node: &Node<'_>) -> Result<(), CatalogError<E>> {
	let mut previous = None;
	for record in node.records() {
		let (key, _) = split_key(id, record)?;
		if previous.is_some_and(|before| compare_keys(before, key) != Ordering::Less) {
			return Err(invalid(id, Corruption::RecordOrder));
		}
		previous = Some(key);
	}
	Ok(())
}

pub(super) fn collates_empty(name: &[u16]) -> bool {
	COLLATION.compare(name, &[]) == Ordering::Equal
}

fn compare_keys(left: Key<'_>, right: Key<'_>) -> Ordering {
	left.parent_or_thread
		.cmp(&right.parent_or_thread)
		.then_with(|| OwnedKey::from_key(left).compare(right))
}

#[derive(Default)]
struct KeyBounds {
	lower: Option<OwnedKey>,
	upper: Option<OwnedKey>,
}

enum NodeSearch {
	Absent,
	Match(usize),
	Descend(NodeId),
}

#[derive(Clone, Copy)]
struct SearchKey<'name> {
	id: CatalogId,
	name: &'name [u16],
}

impl SearchKey<'_> {
	fn compare(&self, right: Key<'_>) -> Ordering {
		self.id
			.cmp(&right.parent_or_thread)
			.then_with(|| COLLATION.compare(self.name, OwnedKey::from_key(right).name()))
	}
}

pub(super) struct OwnedKey {
	id: CatalogId,
	name: [u16; MAX_NAME_UNITS],
	length: usize,
}

impl OwnedKey {
	pub(super) fn from_key(key: Key<'_>) -> Self {
		let mut name = [0; MAX_NAME_UNITS];
		let length = key.name.len();
		for (output, unit) in name.iter_mut().zip(key.name.units()) {
			*output = unit;
		}
		Self {
			id: key.parent_or_thread,
			name,
			length,
		}
	}

	fn name(&self) -> &[u16] {
		&self.name[..self.length]
	}

	pub(super) fn compare(&self, right: Key<'_>) -> Ordering {
		SearchKey {
			id: self.id,
			name: self.name(),
		}
		.compare(right)
	}
}

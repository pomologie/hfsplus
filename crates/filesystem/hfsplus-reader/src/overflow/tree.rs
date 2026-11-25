use crate::alloc::vec::Vec;
use hfsplus_core::{ByteOffset, ReadAt};
use hfsplus_format::{
	Error as FormatError,
	btree::{
		BIG_KEYS_MASK, HEADER_USER_DATA_BYTES, HFS_BTREE_TYPE, Kind, LEAF_HEIGHT, Node, NodeId,
		TreeHeader, VARIABLE_INDEX_KEYS_MASK,
	},
	volume::CatalogId,
};

use super::{
	ForkKind, ForkReader, OverflowError, OverflowLimit, OverflowLimits, RECORD_VALUE_BYTES,
};

const HEADER_PREFIX_BYTES: usize = 34;
const HEADER_USER_DATA_RECORD: usize = 1;
const HEADER_MAP_RECORD: usize = 2;
const MAP_RECORD: usize = 0;
const HEADER_NODE: NodeId = NodeId(0);

const KEY_LENGTH: u16 = 10;
const KEY_BYTES: usize = size_of::<u16>() + KEY_LENGTH as usize;
const KEY_FORK_KIND: usize = size_of::<u16>();
const KEY_CATALOG_ID: usize = KEY_FORK_KIND + 2; // fork type plus padding.
const KEY_START_BLOCK: usize = KEY_CATALOG_ID + size_of::<u32>();
const INDEX_RECORD_BYTES: usize = KEY_BYTES + size_of::<u32>();
const LEAF_RECORD_BYTES: usize = KEY_BYTES + RECORD_VALUE_BYTES;
const MOST_SIGNIFICANT_BIT: u8 = 0x80;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
	id: CatalogId,
	kind: ForkKind,
	start_block: u32,
}

impl Key {
	fn decode(record: &[u8]) -> Result<Self, FormatError> {
		let key = record.get(..KEY_BYTES).ok_or(FormatError::InvalidNode)?;
		let length = key
			.first_chunk::<{ size_of::<u16>() }>()
			.ok_or(FormatError::InvalidNode)?;
		if u16::from_be_bytes(*length) != KEY_LENGTH {
			return Err(FormatError::InvalidNode);
		}
		let kind = match key[KEY_FORK_KIND] {
			value if value == ForkKind::Data as u8 => ForkKind::Data,
			value if value == ForkKind::Resource as u8 => ForkKind::Resource,
			_ => return Err(FormatError::InvalidNode),
		};
		Ok(Self {
			id: CatalogId(read_u32(&key[KEY_CATALOG_ID..])?),
			kind,
			start_block: read_u32(&key[KEY_START_BLOCK..])?,
		})
	}

	fn owner(self) -> (CatalogId, ForkKind) {
		(self.id, self.kind)
	}
}

struct Route {
	child: NodeId,
	lower: Key,
	upper: Option<Key>,
}

pub(super) struct OverflowTree<'volume, S> {
	fork: ForkReader<'volume, S>,
	header: TreeHeader,
	bytes: Vec<u8>,
	header_map: Vec<u8>,
	first_map: Option<NodeId>,
	reads_left: usize,
}

impl<'volume, S: ReadAt> OverflowTree<'volume, S> {
	pub(super) fn open(
		fork: ForkReader<'volume, S>,
		limits: OverflowLimits,
	) -> Result<Self, OverflowError<S::Error>> {
		let mut reads_left = limits.max_metadata_reads;
		let mut prefix = [0; HEADER_PREFIX_BYTES];
		budget_read(&fork, &mut reads_left, ByteOffset(0), &mut prefix)?;
		let node_bytes = TreeHeader::node_size_hint(&prefix)?;
		if node_bytes > limits.max_node_bytes {
			return Err(OverflowError::Limit(OverflowLimit::NodeBytes));
		}
		let mut bytes = Vec::new();
		bytes.try_reserve_exact(usize::from(node_bytes))?;
		bytes.resize(usize::from(node_bytes), 0);
		budget_read(&fork, &mut reads_left, ByteOffset(0), &mut bytes)?;
		let node = Node::decode(&bytes)?;
		let header = TreeHeader::decode(&node, fork.len().0)?;
		let key_attributes = BIG_KEYS_MASK | VARIABLE_INDEX_KEYS_MASK;
		if header.tree_type != HFS_BTREE_TYPE
			|| header.max_key_bytes != KEY_LENGTH
			|| header.attributes & key_attributes != BIG_KEYS_MASK
			|| u64::from(header.total_nodes) * u64::from(node_bytes) != fork.len().0
		{
			return Err(FormatError::InvalidNode.into());
		}
		let first_map = node.forward();
		let map = node
			.record(HEADER_MAP_RECORD)
			.ok_or(FormatError::InvalidNode)?;
		if map.len() != Node::header_map_bytes(bytes.len())?
			|| node.record(HEADER_USER_DATA_RECORD).map(<[u8]>::len) != Some(HEADER_USER_DATA_BYTES)
		{
			return Err(FormatError::InvalidNode.into());
		}
		let mut header_map = Vec::new();
		header_map.try_reserve_exact(map.len())?;
		header_map.extend_from_slice(map);
		if !map_contains(&header_map, 0, HEADER_NODE) {
			return Err(FormatError::InvalidNode.into());
		}
		Ok(Self {
			fork,
			header,
			bytes,
			header_map,
			first_map,
			reads_left,
		})
	}

	pub(super) fn walk(
		&mut self,
		id: CatalogId,
		kind: ForkKind,
		mut continuation: impl FnMut(
			u32,
			&[u8; RECORD_VALUE_BYTES],
		) -> Result<(), OverflowError<S::Error>>,
	) -> Result<(), OverflowError<S::Error>> {
		let target = Key {
			id,
			kind,
			start_block: 0,
		};
		let Some(mut current) = self.seek_first_leaf(target)? else {
			return Ok(());
		};
		let mut previous = None;
		let mut previous_last = None;
		loop {
			let node = Node::decode(&self.bytes)?;
			self.validate_leaf(current, &node)?;
			if previous.is_some() && node.backward() != previous {
				return Err(FormatError::InvalidNode.into());
			}
			let (first, last) = checked_keys(&node, LEAF_RECORD_BYTES)?;
			if previous_last.is_some_and(|key| key >= first) {
				return Err(FormatError::InvalidNode.into());
			}
			for record in node.records() {
				let key = Key::decode(record)?;
				match key.owner().cmp(&target.owner()) {
					core::cmp::Ordering::Less => {}
					core::cmp::Ordering::Equal => {
						let value = record[KEY_BYTES..]
							.try_into()
							.map_err(|_| FormatError::InvalidNode)?;
						continuation(key.start_block, value)?;
					}
					core::cmp::Ordering::Greater => return Ok(()),
				}
			}
			let Some(next) = node.forward() else {
				return Ok(());
			};
			previous = Some(current);
			previous_last = Some(last);
			self.read_node(next)?;
			current = next;
		}
	}

	fn seek_first_leaf(&mut self, target: Key) -> Result<Option<NodeId>, OverflowError<S::Error>> {
		let Some(root) = self.header.root else {
			return Ok(None);
		};
		let mut id = root;
		let mut route: Option<Route> = None;
		for height in (u16::from(LEAF_HEIGHT)..=self.header.depth).rev() {
			self.read_node(id)?;
			let node = Node::decode(&self.bytes)?;
			let leaf = height == u16::from(LEAF_HEIGHT);
			let (kind, width) = if leaf {
				(Kind::Leaf, LEAF_RECORD_BYTES)
			} else {
				(Kind::Index, INDEX_RECORD_BYTES)
			};
			if node.kind() != kind
				|| u16::from(node.height()) != height
				|| (id == root && (node.forward().is_some() || node.backward().is_some()))
				|| !self.valid_links(id, &node)
			{
				return Err(FormatError::InvalidNode.into());
			}
			let (first, last) = checked_keys(&node, width)?;
			if route.as_ref().is_some_and(|bounds| {
				first != bounds.lower || bounds.upper.is_some_and(|upper| last >= upper)
			}) {
				return Err(FormatError::InvalidNode.into());
			}
			if leaf {
				return Ok(Some(id));
			}
			let upper = route.and_then(|bounds| bounds.upper);
			let next = self.child(&node, target, upper)?;
			id = next.child;
			route = Some(next);
		}
		Err(FormatError::InvalidNode.into())
	}

	fn child(
		&self,
		node: &Node<'_>,
		target: Key,
		parent_upper: Option<Key>,
	) -> Result<Route, FormatError> {
		let mut selected = None;
		let mut upper = None;
		for record in node.records() {
			let key = Key::decode(record)?;
			let id = NodeId(read_u32(&record[KEY_BYTES..])?);
			if !self.valid_node_id(id) {
				return Err(FormatError::InvalidNode);
			}
			if selected.is_none() || key <= target {
				selected = Some((id, key));
			} else if upper.is_none() {
				upper = Some(key);
			}
		}
		let (child, lower) = selected.ok_or(FormatError::InvalidNode)?;
		Ok(Route {
			child,
			lower,
			upper: upper.or(parent_upper),
		})
	}

	fn validate_leaf(&self, current: NodeId, node: &Node<'_>) -> Result<(), FormatError> {
		if node.kind() != Kind::Leaf
			|| node.backward().is_none() != (self.header.first_leaf == Some(current))
			|| node.forward().is_none() != (self.header.last_leaf == Some(current))
			|| !self.valid_links(current, node)
		{
			return Err(FormatError::InvalidNode);
		}
		Ok(())
	}

	fn valid_links(&self, from: NodeId, node: &Node<'_>) -> bool {
		let valid = |id| id != from && self.valid_node_id(id);
		let forward = node.forward();
		let backward = node.backward();
		forward.is_none_or(valid)
			&& backward.is_none_or(valid)
			&& (forward.is_none() || forward != backward)
	}

	fn valid_node_id(&self, id: NodeId) -> bool {
		id != HEADER_NODE && id.0 < self.header.total_nodes
	}

	fn read_node(&mut self, id: NodeId) -> Result<(), OverflowError<S::Error>> {
		if !self.valid_node_id(id) || !self.allocated(id)? {
			return Err(FormatError::InvalidNode.into());
		}
		self.read_raw_node(id)
	}

	fn read_raw_node(&mut self, id: NodeId) -> Result<(), OverflowError<S::Error>> {
		if !self.valid_node_id(id) {
			return Err(FormatError::InvalidNode.into());
		}
		let start = ByteOffset(u64::from(id.0) * u64::from(self.header.node_bytes));
		budget_read(&self.fork, &mut self.reads_left, start, &mut self.bytes)
	}

	fn allocated(&mut self, id: NodeId) -> Result<bool, OverflowError<S::Error>> {
		let byte = map_byte(id);
		let mut map_start = self.header_map.len();
		if byte < map_start {
			return Ok(map_contains(&self.header_map, 0, id));
		}
		let mut map = self.first_map;
		let mut seen = Vec::new();
		while let Some(node_id) = map {
			if seen.contains(&node_id) || node_id == id {
				return Err(FormatError::InvalidNode.into());
			}
			seen.try_reserve(1)?;
			seen.push(node_id);
			self.read_raw_node(node_id)?;
			let node = Node::decode(&self.bytes)?;
			if node.kind() != Kind::Map {
				return Err(FormatError::InvalidNode.into());
			}
			let data = node.record(MAP_RECORD).ok_or(FormatError::InvalidNode)?;
			if data.len() != Node::continuation_map_bytes(self.bytes.len())?
				|| !self.valid_links(node_id, &node)
			{
				return Err(FormatError::InvalidNode.into());
			}
			let map_end = map_start
				.checked_add(data.len())
				.ok_or(FormatError::InvalidNode)?;
			if byte < map_end {
				return Ok(map_contains(data, map_start, id));
			}
			map = node.forward();
			map_start = map_end;
		}
		Err(FormatError::InvalidNode.into())
	}
}

fn checked_keys(node: &Node<'_>, record_width: usize) -> Result<(Key, Key), FormatError> {
	let mut first = None;
	let mut last = None;
	for record in node.records() {
		if record.len() != record_width {
			return Err(FormatError::InvalidNode);
		}
		let key = Key::decode(record)?;
		if last.is_some_and(|previous| key <= previous) {
			return Err(FormatError::InvalidNode);
		}
		first.get_or_insert(key);
		last = Some(key);
	}
	first.zip(last).ok_or(FormatError::InvalidNode)
}

fn read_u32(bytes: &[u8]) -> Result<u32, FormatError> {
	bytes
		.first_chunk::<{ size_of::<u32>() }>()
		.copied()
		.map(u32::from_be_bytes)
		.ok_or(FormatError::InvalidNode)
}

fn map_byte(id: NodeId) -> usize {
	(id.0 / u8::BITS) as usize
}

fn map_contains(map: &[u8], map_start: usize, id: NodeId) -> bool {
	let bit = MOST_SIGNIFICANT_BIT >> (id.0 % u8::BITS);
	map_byte(id)
		.checked_sub(map_start)
		.and_then(|byte| map.get(byte))
		.is_some_and(|entry| entry & bit != 0)
}

fn budget_read<S: ReadAt>(
	fork: &ForkReader<'_, S>,
	remaining: &mut usize,
	offset: ByteOffset,
	destination: &mut [u8],
) -> Result<(), OverflowError<S::Error>> {
	*remaining = remaining
		.checked_sub(1)
		.ok_or(OverflowError::Limit(OverflowLimit::MetadataReads))?;
	fork.read_exact_at(offset, destination)
		.map_err(OverflowError::Read)
}

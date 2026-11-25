//! H-READ-04 malformed extent cases and synthetic multi-level lookup.

use hfsplus_core::{ByteLen, ByteOffset, RangeError, ReadAt};
use hfsplus_format::{
	Error as FormatError,
	btree::{
		BIG_KEYS_MASK, HEADER_RECORD_BYTES, HEADER_USER_DATA_BYTES, Kind, LEAF_HEIGHT,
		METADATA_NODE_HEIGHT, MIN_INDEX_HEIGHT, Node, NodeId, NodeLinks, TreeHeader, TreeLayout,
		TreeMetadata, TreeTopology,
	},
	volume::{
		AllocationBlock, CatalogId, Extent, Fork, HEADER_BYTES, Header, PRIMARY_HEADER_OFFSET,
	},
};
use hfsplus_reader::{
	Error, SpecialFile, Volume,
	overflow::{ForkKind, OverflowError, OverflowLimit, OverflowLimits},
};

#[path = "overflow/empty.rs"]
mod empty;

const OPTICAL: &[u8] = include_bytes!("fixtures/optical-volume.bin");
const BLOCK_BYTES: usize = 512;
const VOLUME_BLOCKS: u32 = 128;
const TREE_START: usize = 4 * BLOCK_BYTES;
const TREE_NODES: u32 = 4;
const ROOT: NodeId = NodeId(1);
const LEAF_FIRST: NodeId = NodeId(2);
const LEAF_SECOND: NodeId = NodeId(3);
const FILE_ID: CatalogId = CatalogId(20);
const OTHER_ID: CatalogId = CatalogId(19);
const EMBEDDED_START: u32 = 10;
const FIRST_CONTINUATION_PHYSICAL: u32 = 40;
const LAST_CONTINUATION_PHYSICAL: u32 = 56;
const EXTENT_STEP: u32 = 2;
const GROUP_SIZE: u32 = 8;
const TOTAL_FORK_BLOCKS: u32 = 17;
const FORK_LEN: usize = TOTAL_FORK_BLOCKS as usize * BLOCK_BYTES - 43;

const KEY_LENGTH: u16 = 10;
const KEY_BYTES: usize = KEY_LENGTH as usize + size_of::<u16>();
const LEAF_RECORD_BYTES: usize = KEY_BYTES + 8 * GROUP_SIZE as usize;
const INDEX_RECORD_BYTES: usize = KEY_BYTES + size_of::<u32>();
const NODE_DESCRIPTOR_BYTES: usize = 14;
const KEY_START_BYTE: usize = NODE_DESCRIPTOR_BYTES + KEY_BYTES - size_of::<u32>();

#[test]
fn follows_a_two_level_tree_and_reads_exact_logical_bytes_with_eof() {
	let (image, fork) = synthetic_volume();
	let volume = Volume::open(image.as_slice()).unwrap();
	let resolved = volume
		.open_fork(FILE_ID, ForkKind::Data, &fork, OverflowLimits::default())
		.unwrap();
	assert_eq!(resolved.len(), ByteLen(FORK_LEN as u64));
	let expected = allocation_pattern::<8>(EMBEDDED_START)
		.into_iter()
		.chain(allocation_pattern::<8>(FIRST_CONTINUATION_PHYSICAL))
		.chain([LAST_CONTINUATION_PHYSICAL])
		.flat_map(|byte| std::iter::repeat_n(byte as u8, BLOCK_BYTES))
		.take(FORK_LEN)
		.collect::<Vec<_>>();
	let mut actual = vec![0; FORK_LEN];
	resolved.read_exact_at(ByteOffset(0), &mut actual).unwrap();
	assert_eq!(actual, expected);
	assert!(
		resolved
			.read_exact_at(ByteOffset(FORK_LEN as u64), &mut [])
			.is_ok()
	);
	assert!(matches!(
		resolved.read_exact_at(ByteOffset(FORK_LEN as u64 - 1), &mut [0; 2]),
		Err(Error::Bounds(RangeError::OutsideSource))
	));
}

#[test]
fn rejects_gaps_duplicates_invalid_routes_and_unallocated_nodes() {
	let limits = OverflowLimits::default();
	let (original, fork) = synthetic_volume();
	let leaf_second = node_offset(LEAF_SECOND);
	let index = node_offset(ROOT);

	let mut image = original.clone();
	write_u32(&mut image, leaf_second + KEY_START_BYTE, 2 * GROUP_SIZE + 1);
	let volume = Volume::open(image.as_slice()).unwrap();
	assert!(
		matches!(volume.open_fork(FILE_ID, ForkKind::Data, &fork, limits),
		Err(OverflowError::InvalidStart { expected, actual })
		if expected == 2 * GROUP_SIZE && actual == 2 * GROUP_SIZE + 1)
	);

	let mut image = original.clone();
	write_u32(
		&mut image,
		leaf_second + NODE_DESCRIPTOR_BYTES + KEY_BYTES,
		EMBEDDED_START,
	);
	let volume = Volume::open(image.as_slice()).unwrap();
	assert!(matches!(
		volume.open_fork(FILE_ID, ForkKind::Data, &fork, limits),
		Err(OverflowError::Read(Error::Format(FormatError::InvalidFork)))
	));

	let mut image = original.clone();
	write_u32(
		&mut image,
		index + NODE_DESCRIPTOR_BYTES + KEY_BYTES,
		ROOT.0,
	);
	let volume = Volume::open(image.as_slice()).unwrap();
	assert!(matches!(
		volume.open_fork(FILE_ID, ForkKind::Data, &fork, limits),
		Err(OverflowError::Read(Error::Format(FormatError::InvalidNode)))
	));

	let mut image = original;
	let bitmap = TREE_START + NODE_DESCRIPTOR_BYTES + HEADER_RECORD_BYTES + HEADER_USER_DATA_BYTES;
	image[bitmap] &= !(0x80 >> LEAF_SECOND.0);
	let volume = Volume::open(image.as_slice()).unwrap();
	assert!(matches!(
		volume.open_fork(FILE_ID, ForkKind::Data, &fork, limits),
		Err(OverflowError::Read(Error::Format(FormatError::InvalidNode)))
	));
}

#[test]
fn enforces_metadata_limits_and_selects_the_fork_kind_exactly() {
	let (image, fork) = synthetic_volume();
	let volume = Volume::open(image.as_slice()).unwrap();
	let defaults = OverflowLimits::default();
	for (limits, exhausted) in [
		(
			OverflowLimits {
				max_node_bytes: 256,
				..defaults
			},
			OverflowLimit::NodeBytes,
		),
		(
			OverflowLimits {
				max_metadata_reads: 2,
				..defaults
			},
			OverflowLimit::MetadataReads,
		),
		(
			OverflowLimits {
				max_records: 1,
				..defaults
			},
			OverflowLimit::Records,
		),
	] {
		assert!(
			matches!(volume.open_fork(FILE_ID, ForkKind::Data, &fork, limits),
			Err(OverflowError::Limit(kind)) if kind == exhausted)
		);
	}
	assert!(matches!(
		volume.open_fork(CatalogId::FIRST_USER, ForkKind::Data, &fork, defaults),
		Err(OverflowError::MissingRecord {
			start_block: GROUP_SIZE
		})
	));
	assert!(matches!(
		volume.open_fork(FILE_ID, ForkKind::Resource, &fork, defaults),
		Err(OverflowError::Read(Error::Format(FormatError::InvalidFork)))
	));
}

#[test]
fn visits_allocation_map_continuations_and_rejects_a_self_link() {
	const MAP: NodeId = NodeId(1);
	const LARGE_LAYOUT_BLOCKS: u32 = 4096;
	let header_bytes = Node::header_map_bytes(BLOCK_BYTES).unwrap();
	let leaf = NodeId(header_bytes as u32 * u8::BITS);
	let tree_nodes = leaf.0 + 1;
	let (mut image, header) = volume_with_tree(LARGE_LAYOUT_BLOCKS, tree_nodes);
	let catalog_block = tree_nodes + GROUP_SIZE;
	let last_block = catalog_block + 2 * GROUP_SIZE;
	let tree = TreeHeader::new(
		TreeLayout {
			node_bytes: BLOCK_BYTES as u16,
			max_key_bytes: KEY_LENGTH,
			total_nodes: tree_nodes,
			free_nodes: tree_nodes - 3,
		},
		TreeTopology {
			depth: u16::from(LEAF_HEIGHT),
			root: Some(leaf),
			leaf_records: 1,
			first_leaf: Some(leaf),
			last_leaf: Some(leaf),
		},
		TreeMetadata {
			attributes: BIG_KEYS_MASK,
			..TreeMetadata::default()
		},
	)
	.unwrap();
	let mut map_first = vec![0; header_bytes];
	map_first[0] = 0xc0;
	Node::encode(
		node_bytes(&mut image, NodeId(0)),
		Kind::Header,
		METADATA_NODE_HEIGHT,
		NodeLinks {
			forward: Some(MAP),
			backward: None,
		},
		&[
			&tree.encode_record().unwrap(),
			&[0; HEADER_USER_DATA_BYTES],
			&map_first,
		],
	)
	.unwrap();
	let mut map_later = vec![0; Node::continuation_map_bytes(BLOCK_BYTES).unwrap()];
	map_later[0] = 0x80;
	Node::encode(
		node_bytes(&mut image, MAP),
		Kind::Map,
		METADATA_NODE_HEIGHT,
		NodeLinks::default(),
		&[&map_later],
	)
	.unwrap();
	Node::encode(
		node_bytes(&mut image, leaf),
		Kind::Leaf,
		LEAF_HEIGHT,
		NodeLinks::default(),
		&[&leaf_record(
			key(FILE_ID, ForkKind::Data, GROUP_SIZE),
			[last_block],
		)],
	)
	.unwrap();
	let fork = embedded_descriptor(catalog_block, GROUP_SIZE + 1, header.geometry);
	let volume = Volume::open(image.as_slice()).unwrap();
	let file = volume
		.open_fork(FILE_ID, ForkKind::Data, &fork, OverflowLimits::default())
		.unwrap();
	let mut actual = [0; 1];
	file.read_exact_at(
		ByteOffset(u64::from(GROUP_SIZE) * BLOCK_BYTES as u64),
		&mut actual,
	)
	.unwrap();
	assert_eq!(actual, [last_block as u8]);

	write_u32(&mut image, node_offset(MAP), MAP.0);
	let volume = Volume::open(image.as_slice()).unwrap();
	assert!(matches!(
		volume.open_fork(FILE_ID, ForkKind::Data, &fork, OverflowLimits::default()),
		Err(OverflowError::Read(Error::Format(FormatError::InvalidNode)))
	));
}

#[test]
fn rejects_extents_tree_self_overflow_before_opening_it() {
	let (mut image, mut header) = volume_with_tree(VOLUME_BLOCKS, TREE_NODES);
	header.forks.extents = embedded_descriptor(EMBEDDED_START, GROUP_SIZE + 1, header.geometry);
	write_headers(&mut image, &header);
	let volume = Volume::open(image.as_slice()).unwrap();
	assert!(matches!(
		volume.open_resolved_special_fork(SpecialFile::Extents, OverflowLimits::default()),
		Err(OverflowError::BootstrapOverflow)
	));
}

fn volume_with_tree(volume_blocks: u32, tree_nodes: u32) -> (Vec<u8>, Header) {
	let mut image = vec![0; volume_blocks as usize * BLOCK_BYTES];
	for (index, block) in image
		.as_chunks_mut::<BLOCK_BYTES>()
		.0
		.iter_mut()
		.enumerate()
	{
		block.fill(index as u8);
	}
	let native = Volume::open(OPTICAL).unwrap();
	let mut header: Header = *native.header();
	let geometry = hfsplus_format::volume::Geometry::new(
		BLOCK_BYTES as u32,
		volume_blocks,
		image.len() as u64,
	)
	.unwrap();
	header.geometry = geometry;
	header.free_blocks = volume_blocks / 2;
	header.next_allocation = AllocationBlock(volume_blocks / 2);
	header.forks.allocation = Fork::empty();
	header.forks.catalog = Fork::empty();
	header.forks.attributes = Fork::empty();
	header.forks.startup = Fork::empty();
	header.forks.extents = Fork::new(
		u64::from(tree_nodes) * BLOCK_BYTES as u64,
		0,
		tree_nodes,
		&[Extent {
			start: AllocationBlock(TREE_START as u32 / BLOCK_BYTES as u32),
			blocks: tree_nodes,
		}],
		geometry,
	)
	.unwrap();
	write_headers(&mut image, &header);
	(image, header)
}

fn embedded_descriptor(start: u32, total: u32, geometry: hfsplus_format::volume::Geometry) -> Fork {
	let data_extents = allocation_pattern::<8>(start).map(|block| Extent {
		start: AllocationBlock(block),
		blocks: 1,
	});
	Fork::new(
		u64::from(total) * BLOCK_BYTES as u64,
		0,
		total,
		&data_extents,
		geometry,
	)
	.unwrap()
}

fn write_headers(image: &mut [u8], header: &Header) {
	let encoded = header.encode().unwrap();
	let primary = PRIMARY_HEADER_OFFSET as usize;
	let alternate = header.geometry.alternate_header_offset() as usize;
	image[primary..primary + HEADER_BYTES].copy_from_slice(&encoded);
	image[alternate..alternate + HEADER_BYTES].copy_from_slice(&encoded);
}

fn synthetic_volume() -> (Vec<u8>, Fork) {
	let (mut image, header) = volume_with_tree(VOLUME_BLOCKS, TREE_NODES);
	let mut fork = embedded_descriptor(EMBEDDED_START, TOTAL_FORK_BLOCKS, header.geometry);
	fork.logical_bytes = FORK_LEN as u64;
	write_tree(&mut image);
	(image, fork)
}

fn write_tree(image: &mut [u8]) {
	let header = TreeHeader::new(
		TreeLayout {
			node_bytes: BLOCK_BYTES as u16,
			max_key_bytes: KEY_LENGTH,
			total_nodes: TREE_NODES,
			free_nodes: 0,
		},
		TreeTopology {
			depth: u16::from(MIN_INDEX_HEIGHT),
			root: Some(ROOT),
			leaf_records: 4,
			first_leaf: Some(LEAF_FIRST),
			last_leaf: Some(LEAF_SECOND),
		},
		TreeMetadata {
			attributes: BIG_KEYS_MASK,
			..TreeMetadata::default()
		},
	)
	.unwrap();
	let mut map = vec![0; Node::header_map_bytes(BLOCK_BYTES).unwrap()];
	map[0] = 0xf0;
	Node::encode(
		node_bytes(image, NodeId(0)),
		Kind::Header,
		METADATA_NODE_HEIGHT,
		NodeLinks::default(),
		&[
			&header.encode_record().unwrap(),
			&[0; HEADER_USER_DATA_BYTES],
			&map,
		],
	)
	.unwrap();

	let first = key(OTHER_ID, ForkKind::Data, GROUP_SIZE);
	let subsequent = key(FILE_ID, ForkKind::Data, 2 * GROUP_SIZE);
	Node::encode(
		node_bytes(image, ROOT),
		Kind::Index,
		MIN_INDEX_HEIGHT,
		NodeLinks::default(),
		&[
			&index_record(first, LEAF_FIRST),
			&index_record(subsequent, LEAF_SECOND),
		],
	)
	.unwrap();

	let first_data = key(FILE_ID, ForkKind::Data, GROUP_SIZE);
	Node::encode(
		node_bytes(image, LEAF_FIRST),
		Kind::Leaf,
		LEAF_HEIGHT,
		NodeLinks {
			forward: Some(LEAF_SECOND),
			backward: None,
		},
		&[
			&leaf_record(first, [58]),
			&leaf_record(
				first_data,
				allocation_pattern::<8>(FIRST_CONTINUATION_PHYSICAL),
			),
		],
	)
	.unwrap();

	let resource = key(FILE_ID, ForkKind::Resource, GROUP_SIZE);
	Node::encode(
		node_bytes(image, LEAF_SECOND),
		Kind::Leaf,
		LEAF_HEIGHT,
		NodeLinks {
			forward: None,
			backward: Some(LEAF_FIRST),
		},
		&[
			&leaf_record(subsequent, [LAST_CONTINUATION_PHYSICAL]),
			&leaf_record(resource, [60]),
		],
	)
	.unwrap();
}

fn allocation_pattern<const N: usize>(first: u32) -> [u32; N] {
	std::array::from_fn(|index| first + EXTENT_STEP * index as u32)
}

fn node_offset(id: NodeId) -> usize {
	TREE_START + id.0 as usize * BLOCK_BYTES
}

fn node_bytes(image: &mut [u8], id: NodeId) -> &mut [u8] {
	let start = node_offset(id);
	&mut image[start..start + BLOCK_BYTES]
}

fn key(id: CatalogId, kind: ForkKind, block: u32) -> [u8; KEY_BYTES] {
	let mut bytes = [0; KEY_BYTES];
	bytes[..2].copy_from_slice(&KEY_LENGTH.to_be_bytes());
	bytes[2] = kind as u8;
	write_u32(&mut bytes, 4, id.0);
	write_u32(&mut bytes, 8, block);
	bytes
}

fn index_record(key: [u8; KEY_BYTES], child: NodeId) -> [u8; INDEX_RECORD_BYTES] {
	let mut bytes = [0; INDEX_RECORD_BYTES];
	bytes[..KEY_BYTES].copy_from_slice(&key);
	write_u32(&mut bytes, KEY_BYTES, child.0);
	bytes
}

fn leaf_record<const N: usize>(key: [u8; KEY_BYTES], blocks: [u32; N]) -> [u8; LEAF_RECORD_BYTES] {
	let mut bytes = [0; LEAF_RECORD_BYTES];
	bytes[..KEY_BYTES].copy_from_slice(&key);
	for (index, block) in blocks.into_iter().enumerate() {
		write_u32(&mut bytes, KEY_BYTES + index * 8, block);
		write_u32(&mut bytes, KEY_BYTES + index * 8 + 4, 1);
	}
	bytes
}

fn write_u32(bytes: &mut [u8], at: usize, value: u32) {
	bytes[at..at + size_of::<u32>()].copy_from_slice(&value.to_be_bytes());
}

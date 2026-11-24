use hfsplus_core::{ByteOffset, ReadAt};
use hfsplus_format::{
	btree::{
		BIG_KEYS_MASK, HEADER_USER_DATA_BYTES, Kind, LEAF_HEIGHT, METADATA_NODE_HEIGHT, Node,
		NodeId, NodeLinks, TreeHeader, TreeLayout, TreeMetadata, TreeTopology,
	},
	volume::{AllocationBlock, Extent, Fork, PRIMARY_HEADER_OFFSET},
};
use hfsplus_reader::{
	Volume,
	catalog::{CatalogLimits, HardLinkLimits},
	overflow::{ForkKind, OverflowError, OverflowLimits},
};

use super::{FIRST_INODE, FIRST_STUB, Fixture, NATIVE, value_offset};

const BLOCK_BYTES: usize = 4096;
const EMBEDDED_EXTENTS: u32 = 8;
const EMBEDDED_START: u32 = 3072;
const CONTINUATION_BLOCK: u32 = EMBEDDED_START + EMBEDDED_EXTENTS;
const TREE_BLOCK: u32 = CONTINUATION_BLOCK + 1;
const LEAF: NodeId = NodeId(1);
const TREE_NODES: u32 = 2;
const HEADER_AND_LEAF_MAP_BITS: u8 = 0xc0;
const FILE_DATA_FORK_OFFSET: u64 = 88;
const EXTENTS_KEY_LENGTH: u16 = 10;
const EXTRAS_KEY_BYTES: usize = 2;
const BYTES_PER_EXTENT: usize = 2 * size_of::<u32>();
const EXTENTS_RECORD_BYTES: usize =
	EXTENTS_KEY_LENGTH as usize + size_of::<u16>() + EMBEDDED_EXTENTS as usize * BYTES_PER_EXTENT;
const LOGICAL_CONTINUATION: u64 = EMBEDDED_EXTENTS as u64 * BLOCK_BYTES as u64;
const SAMPLE: u8 = 0xab;

#[test]
fn opens_overflow_extents_keyed_by_the_inode_instead_of_the_visible_stub() {
	let mut fixture = Fixture::decode(NATIVE);
	let mut header = *Volume::open(&fixture).unwrap().header();
	let fork = with_continuation(&mut fixture, &mut header);
	let volume = Volume::open(fixture).unwrap();
	let limits = OverflowLimits::default();
	assert!(matches!(
		volume.open_fork(FIRST_STUB, ForkKind::Data, &fork, limits),
		Err(OverflowError::MissingRecord {
			start_block: EMBEDDED_EXTENTS
		})
	));

	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	let alias = catalog
		.regular_file(FIRST_STUB, HardLinkLimits::default())
		.unwrap()
		.unwrap();
	let data = alias.open_stored_fork(ForkKind::Data, limits).unwrap();
	let mut observed = [0];
	data.read_exact_at(ByteOffset(LOGICAL_CONTINUATION), &mut observed)
		.unwrap();
	assert_eq!(observed, [SAMPLE]);
}

fn with_continuation(fixture: &mut Fixture, header: &mut hfsplus_format::volume::Header) -> Fork {
	let extents = (EMBEDDED_START..CONTINUATION_BLOCK)
		.map(|block| Extent {
			start: AllocationBlock(block),
			blocks: 1,
		})
		.collect::<Vec<_>>();
	let fork = Fork::new(
		LOGICAL_CONTINUATION + 1,
		0,
		EMBEDDED_EXTENTS + 1,
		&extents,
		header.geometry,
	)
	.unwrap();
	let record = value_offset(fixture, FIRST_INODE);
	fixture.replace(
		record + FILE_DATA_FORK_OFFSET,
		fork.encode(header.geometry).unwrap(),
	);
	header.forks.extents = Fork::new(
		u64::from(TREE_NODES) * BLOCK_BYTES as u64,
		0,
		TREE_NODES,
		&[Extent {
			start: AllocationBlock(TREE_BLOCK),
			blocks: TREE_NODES,
		}],
		header.geometry,
	)
	.unwrap();
	let header_bytes = header.encode().unwrap();
	fixture.replace(PRIMARY_HEADER_OFFSET, header_bytes);
	fixture.replace(header.geometry.alternate_header_offset(), header_bytes);
	fixture
		.slices
		.push((u64::from(TREE_BLOCK) * BLOCK_BYTES as u64, extent_tree()));
	fixture.slices.push((
		u64::from(CONTINUATION_BLOCK) * BLOCK_BYTES as u64,
		vec![SAMPLE],
	));
	fork
}

fn extent_tree() -> Vec<u8> {
	let header = TreeHeader::new(
		TreeLayout {
			node_bytes: BLOCK_BYTES as u16,
			max_key_bytes: EXTENTS_KEY_LENGTH,
			total_nodes: TREE_NODES,
			free_nodes: 0,
		},
		TreeTopology {
			depth: u16::from(LEAF_HEIGHT),
			root: Some(LEAF),
			first_leaf: Some(LEAF),
			last_leaf: Some(LEAF),
			leaf_records: 1,
		},
		TreeMetadata {
			attributes: BIG_KEYS_MASK,
			..TreeMetadata::default()
		},
	)
	.unwrap();
	let mut map = vec![0; Node::header_map_bytes(BLOCK_BYTES).unwrap()];
	map[0] = HEADER_AND_LEAF_MAP_BITS;
	let mut tree = vec![0; BLOCK_BYTES * TREE_NODES as usize];
	let (first, leaf) = tree.split_at_mut(BLOCK_BYTES);
	Node::encode(
		first,
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
	Node::encode(
		leaf,
		Kind::Leaf,
		LEAF_HEIGHT,
		NodeLinks::default(),
		&[&extent_record()],
	)
	.unwrap();
	tree
}

fn extent_record() -> [u8; EXTENTS_RECORD_BYTES] {
	let mut record = [0; EXTENTS_RECORD_BYTES];
	let width = size_of::<u32>();
	let first_id = size_of::<u16>() + EXTRAS_KEY_BYTES;
	record[..size_of::<u16>()].copy_from_slice(&EXTENTS_KEY_LENGTH.to_be_bytes());
	record[first_id..first_id + width].copy_from_slice(&FIRST_INODE.0.to_be_bytes());
	let first_value = first_id + 2 * width;
	record[first_id + width..first_value].copy_from_slice(&EMBEDDED_EXTENTS.to_be_bytes());
	record[first_value..first_value + width].copy_from_slice(&CONTINUATION_BLOCK.to_be_bytes());
	record[first_value + width..first_value + 2 * width].copy_from_slice(&1u32.to_be_bytes());
	record
}

//! Bounds and key ordering of retained Apple catalog, extent and attribute nodes.

use std::cmp::Ordering;

use hfsplus_core::names::NameCollation;
use hfsplus_format::{
	attributes, btree, catalog, extents,
	volume::{AllocationBlock, Extent, Geometry, Header},
};

const CATALOG: &[u8] = include_bytes!("fixtures/native-catalog-volume.bin");
const ATTRIBUTES: &[u8] = include_bytes!("fixtures/native-attribute-tree.bin");
const EXTENTS: &[u8] = include_bytes!("fixtures/native-overflow.bin");
const CATALOG_HEADER_START: usize = 1024;
const CATALOG_TREE_START: usize = 2048;
const CATALOG_NODE_BYTES: usize = 4096;
const CATALOG_NODES: usize = 18;
const ATTRIBUTE_VOLUME_BYTES: u64 = 16_777_216;
const ATTRIBUTE_TREE_START: usize = 1024;
const ATTRIBUTE_NODE_BYTES: usize = 8192;
const EXTENT_LEAF_START: usize = 5178;
const EXTENT_NODE_BYTES: usize = 4096;
const DESCRIPTOR_BYTES: usize = 14;
const RECORD_COUNT_OFFSET: usize = 10;
const OFFSET_BYTES: usize = size_of::<u16>();
const CHILD_BYTES: usize = size_of::<u32>();
const EXPECTED_NODES: usize = 21;
const EXPECTED_RECORDS: usize = 228;
const EXPECTED_PAIRS: usize = 3517;

#[derive(Default)]
struct Cases {
	nodes: usize,
	records: usize,
	pairs: usize,
}

#[test]
fn retained_packed_nodes_match_record_ranges_and_key_order() {
	let mut cases = Cases::default();
	let catalog_geometry = Header::decode(&CATALOG[CATALOG_HEADER_START..], CATALOG.len() as u64)
		.unwrap()
		.geometry;
	let catalog_end = CATALOG_TREE_START + CATALOG_NODES * CATALOG_NODE_BYTES;
	for bytes in CATALOG[CATALOG_TREE_START..catalog_end]
		.as_chunks::<CATALOG_NODE_BYTES>()
		.0
		.iter()
		.skip(1)
	{
		check_node(
			&mut cases,
			bytes,
			|kind, record| catalog_key(kind, record, catalog_geometry),
			compare_catalog,
		);
	}
	let attribute_geometry = Header::decode(ATTRIBUTES, ATTRIBUTE_VOLUME_BYTES)
		.unwrap()
		.geometry;
	for bytes in ATTRIBUTES[ATTRIBUTE_TREE_START..]
		.as_chunks::<ATTRIBUTE_NODE_BYTES>()
		.0
		.iter()
		.skip(1)
	{
		check_node(
			&mut cases,
			bytes,
			|kind, record| attribute_key(kind, record, attribute_geometry),
			Ord::cmp,
		);
	}
	let extent_leaf = &EXTENTS[EXTENT_LEAF_START..EXTENT_LEAF_START + EXTENT_NODE_BYTES];
	check_node(&mut cases, extent_leaf, extent_key, Ord::cmp);
	assert_eq!(
		(cases.nodes, cases.records, cases.pairs),
		(EXPECTED_NODES, EXPECTED_RECORDS, EXPECTED_PAIRS)
	);
	println!(
		"HFS+ property cases checked: {}",
		cases.records + cases.pairs
	);
}

fn check_node<'a, Key>(
	cases: &mut Cases,
	bytes: &'a [u8],
	key: impl Fn(btree::Kind, &'a [u8]) -> Key,
	compare: impl Fn(&Key, &Key) -> Ordering,
) {
	let node = btree::Node::decode(bytes).unwrap();
	assert!(matches!(
		node.kind(),
		btree::Kind::Leaf | btree::Kind::Index
	));
	let records = usize::from(read_u16(bytes, RECORD_COUNT_OFFSET));
	let table_start = bytes.len() - (records + 1) * OFFSET_BYTES;
	let offsets: Vec<_> = bytes[table_start..]
		.rchunks_exact(OFFSET_BYTES)
		.map(|word| usize::from(read_u16(word, 0)))
		.collect();
	assert_eq!(offsets[0], DESCRIPTOR_BYTES);
	assert!(offsets[records] <= table_start);
	let mut keys = Vec::with_capacity(records);
	for (ordinal, bounds) in offsets.windows(2).enumerate() {
		let [start, end] = <[usize; 2]>::try_from(bounds).unwrap();
		assert!(start < end);
		let expected = &bytes[start..end];
		assert!(std::ptr::eq(node.record(ordinal).unwrap(), expected));
		keys.push(key(node.kind(), expected));
		cases.records += 1;
	}
	assert_eq!(node.record(records), None);
	for (index, left) in keys.iter().enumerate() {
		for right in &keys[index + 1..] {
			assert_eq!(compare(left, right), Ordering::Less);
			cases.pairs += 1;
		}
	}
	cases.nodes += 1;
}

fn catalog_key(kind: btree::Kind, raw: &[u8], geometry: Geometry) -> (u32, Vec<u16>) {
	const PARENT_OFFSET: usize = 2;
	const NAME_OFFSET: usize = 8;
	let (key, body) = catalog::Key::split_leaf(raw).unwrap();
	let end = usize::from(read_u16(raw, 0)) + OFFSET_BYTES;
	assert!(key.name.units().eq(wire_name(&raw[NAME_OFFSET..end])));
	assert_eq!(key.parent_or_thread.0, read_u32(raw, PARENT_OFFSET));
	assert_eq!(body, &raw[end..]);
	if kind == btree::Kind::Index {
		assert_eq!(body.len(), CHILD_BYTES);
	} else {
		assert_eq!(catalog::Record::decode(raw, geometry).unwrap().key, key);
	}
	(key.parent_or_thread.0, key.name.units().collect())
}

fn compare_catalog(left: &(u32, Vec<u16>), right: &(u32, Vec<u16>)) -> Ordering {
	left.0
		.cmp(&right.0)
		.then_with(|| NameCollation::CaseFolded.compare(&left.1, &right.1))
}

fn attribute_key(kind: btree::Kind, raw: &[u8], geometry: Geometry) -> attributes::Key<'_> {
	const OWNER_OFFSET: usize = 4;
	const START_BLOCK_OFFSET: usize = 8;
	const NAME_OFFSET: usize = 14;
	let (key, body) = attributes::Key::split(raw).unwrap();
	let end = usize::from(read_u16(raw, 0)) + OFFSET_BYTES;
	assert!(key.name.units().eq(wire_name(&raw[NAME_OFFSET..end])));
	assert_eq!(key.file_id.0, read_u32(raw, OWNER_OFFSET));
	assert_eq!(key.start_block, read_u32(raw, START_BLOCK_OFFSET));
	assert_eq!(body, &raw[end..]);
	if kind == btree::Kind::Index {
		assert_eq!(body.len(), CHILD_BYTES);
	} else {
		assert_eq!(attributes::Record::decode(raw, geometry).unwrap().key, key);
	}
	key
}

fn extent_key(kind: btree::Kind, raw: &[u8]) -> extents::Key {
	const FORK_KIND_OFFSET: usize = 2;
	const OWNER_OFFSET: usize = 4;
	const START_BLOCK_OFFSET: usize = 8;
	const EXTENT_START: usize = 12;
	const EXTENT_WIDTH: usize = 8;
	assert_eq!(kind, btree::Kind::Leaf);
	let key = extents::Key::decode(raw).unwrap();
	assert_eq!(key.id.0, read_u32(raw, OWNER_OFFSET));
	assert_eq!(key.start_block, read_u32(raw, START_BLOCK_OFFSET));
	assert_eq!(key.kind as u8, raw[FORK_KIND_OFFSET]);
	let allocated: Vec<_> = raw[EXTENT_START..]
		.as_chunks::<EXTENT_WIDTH>()
		.0
		.iter()
		.map(|pair| Extent {
			start: AllocationBlock(read_u32(pair, 0)),
			blocks: read_u32(pair, CHILD_BYTES),
		})
		.take_while(|extent| extent.blocks != 0)
		.collect();
	assert_eq!(key.encode_record(&allocated).unwrap(), raw);
	key
}

fn wire_name(bytes: &[u8]) -> impl Iterator<Item = u16> + '_ {
	bytes
		.as_chunks::<OFFSET_BYTES>()
		.0
		.iter()
		.copied()
		.map(u16::from_be_bytes)
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
	u16::from_be_bytes(bytes[offset..][..size_of::<u16>()].try_into().unwrap())
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
	u32::from_be_bytes(bytes[offset..][..size_of::<u32>()].try_into().unwrap())
}

#![no_main]
#![forbid(unsafe_code)]

use hfsplus_format::{
	attributes, btree, catalog, extents,
	volume::{AllocationBlock, CatalogId, Extent},
};
use libfuzzer_sys::fuzz_target;

const NODE_SIZES: [usize; 7] = [512, 1024, 2048, 4096, 8192, 16_384, 32_768];
const DESCRIPTOR_BYTES: usize = 14;
const WORD_BYTES: usize = size_of::<u16>();
const NODE_KIND_OFFSET: usize = 8;
const NODE_COUNT_OFFSET: usize = 10;
const INDEX_TAG: u8 = 0;
const LEAF_TAG: u8 = 0xff;
const INDEX_HEIGHT: u8 = 2;
const LEAF_HEIGHT: u8 = 1;
const MAX_RECORDS: usize = 8;
const MIN_RECORDS: usize = 2;
const EXTENT_SLOTS: usize = 8;
const EXTENT_KEY_BYTES: usize = 12;
const EXTENT_RECORD_BYTES: usize = EXTENT_KEY_BYTES + EXTENT_SLOTS * 2 * size_of::<u32>();
const CATALOG_KEY_PREFIX_BYTES: usize = 8;
const ATTR_KEY_PREFIX_BYTES: usize = 14;
const ATTR_INLINE_PREFIX_BYTES: usize = 16;
const CHILD_BYTES: usize = size_of::<u32>();
const FIRST_OWNER: u32 = 20;
const FIRST_CHILD: u32 = 2;
const FIRST_PAYLOAD_BLOCK: u32 = 128;
const BLOCKS_PER_DESCRIPTOR: u32 = 8;
const CATALOG_NAME_LIMIT: usize = 255;
const ATTRIBUTE_NAME_LIMIT: usize = 127;
const INLINE_VALUE_LIMIT: usize = 3802;
const INLINE_VALUE_TAG: u32 = 0x10;
const INLINE_RESERVED_BYTES: usize = 2 * size_of::<u32>();
const CATALOG_ASCII_BASE: u16 = b'a' as u16;

fuzz_target!(|selection: &[u8]| {
	let selector = |index| selection.get(index).copied().unwrap_or_default();
	let bytes = NODE_SIZES[usize::from(selector(0)) % NODE_SIZES.len()];
	let largest_count = ((bytes - DESCRIPTOR_BYTES - WORD_BYTES)
		/ (EXTENT_RECORD_BYTES + WORD_BYTES))
		.min(MAX_RECORDS);
	let count = MIN_RECORDS + usize::from(selector(1)) % (largest_count + 1 - MIN_RECORDS);
	let budget = (bytes - DESCRIPTOR_BYTES - (count + 1) * WORD_BYTES) / count;
	let catalog_name = name(
		selector(2),
		((budget - CATALOG_KEY_PREFIX_BYTES - CHILD_BYTES) / WORD_BYTES).min(CATALOG_NAME_LIMIT),
	);
	let attribute_name = name(
		selector(2),
		((budget - ATTR_KEY_PREFIX_BYTES - ATTR_INLINE_PREFIX_BYTES) / WORD_BYTES)
			.min(ATTRIBUTE_NAME_LIMIT),
	);
	let inline_budget = budget
		- ATTR_KEY_PREFIX_BYTES
		- attribute_name.len() * WORD_BYTES
		- ATTR_INLINE_PREFIX_BYTES;
	let inline_bytes = scaled(
		selector(3),
		(inline_budget / WORD_BYTES * WORD_BYTES).min(INLINE_VALUE_LIMIT),
	);
	let slots = 1 + usize::from(selector(3)) % EXTENT_SLOTS;
	let catalog_rows = (0..count)
		.map(|ordinal| catalog_index(ordinal, &catalog_name))
		.collect();
	let extent_rows = (0..count)
		.map(|ordinal| extent_leaf(ordinal, slots, selector(2)))
		.collect();
	let attribute_rows = (0..count)
		.map(|ordinal| attribute_leaf(ordinal, &attribute_name, inline_bytes, selector(3)))
		.collect();
	check_node(bytes, btree::Kind::Index, catalog_rows);
	check_node(bytes, btree::Kind::Leaf, extent_rows);
	check_node(bytes, btree::Kind::Leaf, attribute_rows);
});

fn scaled(selector: u8, limit: usize) -> usize {
	usize::from(selector) * limit / usize::from(u8::MAX)
}

fn name(selector: u8, limit: usize) -> Vec<u16> {
	let first = CATALOG_ASCII_BASE + u16::from(selector % 26);
	vec![first; 1 + scaled(selector, limit - 1)]
}

fn owner(ordinal: usize) -> CatalogId {
	CatalogId(FIRST_OWNER + u32::try_from(ordinal).unwrap())
}

fn catalog_index(ordinal: usize, name: &[u16]) -> Vec<u8> {
	let owner = owner(ordinal);
	let child = FIRST_CHILD + u32::try_from(ordinal).unwrap();
	let key_bytes = CATALOG_KEY_PREFIX_BYTES + name.len() * WORD_BYTES;
	let mut production = vec![u8::MAX; key_bytes + CHILD_BYTES];
	let written = catalog::Key::encode(owner, name, &mut production[..key_bytes]).unwrap();
	assert_eq!(written, key_bytes);
	production[key_bytes..].copy_from_slice(&child.to_be_bytes());
	let mut wire = key_length(key_bytes);
	wire.extend(owner.0.to_be_bytes());
	write_name(&mut wire, name);
	wire.extend(child.to_be_bytes());
	assert_eq!(production, wire);
	let (actual, stored_child) = catalog::Key::split_leaf(&wire).unwrap();
	assert_eq!(actual.parent_or_thread, owner);
	assert!(actual.name.units().eq(name.iter().copied()));
	assert_eq!(stored_child, child.to_be_bytes());
	production
}

fn extent_leaf(ordinal: usize, slots: usize, selector: u8) -> Vec<u8> {
	let kind = if selector & 1 == 0 {
		extents::ForkKind::Data
	} else {
		extents::ForkKind::Resource
	};
	let ordinal = u32::try_from(ordinal).unwrap();
	let owner = CatalogId(FIRST_OWNER + ordinal);
	let logical = (ordinal + 1) * BLOCKS_PER_DESCRIPTOR;
	let key = extents::Key {
		id: owner,
		kind,
		start_block: logical,
	};
	let first = FIRST_PAYLOAD_BLOCK
		+ ordinal * u32::try_from(EXTENT_SLOTS).unwrap() * BLOCKS_PER_DESCRIPTOR;
	let physical: Vec<_> = (0..u32::try_from(slots).unwrap())
		.map(|index| Extent {
			start: AllocationBlock(first + index * BLOCKS_PER_DESCRIPTOR),
			blocks: BLOCKS_PER_DESCRIPTOR,
		})
		.collect();
	let production = key.encode_record(&physical).unwrap();
	let mut wire = key_length(EXTENT_KEY_BYTES);
	wire.extend([
		match kind {
			extents::ForkKind::Data => 0,
			extents::ForkKind::Resource => 0xff,
		},
		0,
	]);
	wire.extend(owner.0.to_be_bytes());
	wire.extend(logical.to_be_bytes());
	for extent in physical {
		wire.extend(extent.start.0.to_be_bytes());
		wire.extend(extent.blocks.to_be_bytes());
	}
	wire.resize(EXTENT_RECORD_BYTES, 0);
	assert_eq!(production, wire.as_slice());
	assert_eq!(extents::Key::decode(&production).unwrap(), key);
	production.to_vec()
}

fn attribute_leaf(ordinal: usize, name: &[u16], value_bytes: usize, selector: u8) -> Vec<u8> {
	let owner = owner(ordinal);
	let value: Vec<_> = (u8::MIN..=u8::MAX)
		.cycle()
		.take(value_bytes)
		.map(|offset| selector.wrapping_add(offset))
		.collect();
	let key_bytes = ATTR_KEY_PREFIX_BYTES + name.len() * WORD_BYTES;
	let value_padding = value.len() % WORD_BYTES;
	let mut production =
		vec![u8::MAX; key_bytes + ATTR_INLINE_PREFIX_BYTES + value.len() + value_padding];
	let written = attributes::InlineRecord::encode(owner, name, &value, &mut production).unwrap();
	assert_eq!(written, production.len());
	let mut wire = key_length(key_bytes);
	wire.extend(0u16.to_be_bytes());
	wire.extend(owner.0.to_be_bytes());
	wire.extend(0u32.to_be_bytes());
	write_name(&mut wire, name);
	wire.extend(INLINE_VALUE_TAG.to_be_bytes());
	wire.extend([0; INLINE_RESERVED_BYTES]);
	wire.extend(u32::try_from(value.len()).unwrap().to_be_bytes());
	wire.extend(&value);
	wire.resize(wire.len() + value_padding, 0);
	assert_eq!(production, wire);
	let decoded = attributes::InlineRecord::decode(&production).unwrap();
	assert_eq!(decoded.key.file_id, owner);
	assert!(decoded.key.name.units().eq(name.iter().copied()));
	assert_eq!(decoded.value, value);
	production
}

fn key_length(key_bytes: usize) -> Vec<u8> {
	u16::try_from(key_bytes - WORD_BYTES)
		.unwrap()
		.to_be_bytes()
		.to_vec()
}

fn write_name(wire: &mut Vec<u8>, units: &[u16]) {
	wire.extend(u16::try_from(units.len()).unwrap().to_be_bytes());
	wire.extend(units.iter().flat_map(|unit| unit.to_be_bytes()));
}

fn check_node(bytes: usize, kind: btree::Kind, records: Vec<Vec<u8>>) {
	let (tag, height) = match kind {
		btree::Kind::Index => (INDEX_TAG, INDEX_HEIGHT),
		btree::Kind::Leaf => (LEAF_TAG, LEAF_HEIGHT),
		other => panic!("unexpected generated node kind {other:?}"),
	};
	let capacity = bytes - DESCRIPTOR_BYTES - (records.len() + 1) * WORD_BYTES;
	assert_eq!(
		btree::Node::record_capacity(bytes, records.len()).unwrap(),
		capacity
	);
	let mut wire = vec![0; bytes];
	wire[NODE_KIND_OFFSET..NODE_COUNT_OFFSET].copy_from_slice(&[tag, height]);
	wire[NODE_COUNT_OFFSET..NODE_COUNT_OFFSET + WORD_BYTES]
		.copy_from_slice(&u16::try_from(records.len()).unwrap().to_be_bytes());
	let mut position = DESCRIPTOR_BYTES;
	let mut ranges = Vec::with_capacity(records.len());
	for record in &records {
		let next = position + record.len();
		wire[position..next].copy_from_slice(record);
		ranges.push(position..next);
		position = next;
	}
	let offsets = ranges.iter().map(|range| range.start).chain([position]);
	for (ordinal, offset) in offsets.enumerate() {
		let word = bytes - (ordinal + 1) * WORD_BYTES;
		wire[word..word + WORD_BYTES]
			.copy_from_slice(&u16::try_from(offset).unwrap().to_be_bytes());
	}
	let borrowed: Vec<_> = records.iter().map(Vec::as_slice).collect();
	let mut production = vec![u8::MAX; bytes];
	btree::Node::encode(
		&mut production,
		kind,
		height,
		btree::NodeLinks::default(),
		&borrowed,
	)
	.unwrap();
	assert_eq!(production, wire);
	let node = btree::Node::decode(&production).unwrap();
	assert_eq!(
		(node.kind(), node.height(), node.forward(), node.backward()),
		(kind, height, None, None)
	);
	for (ordinal, range) in ranges.into_iter().enumerate() {
		assert!(std::ptr::eq(
			node.record(ordinal).unwrap(),
			&production[range]
		));
	}
	assert_eq!(node.record(records.len()), None);
}

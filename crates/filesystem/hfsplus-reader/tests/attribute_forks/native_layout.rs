use hfsplus_format::attributes::{EXTENTS_PER_RECORD, Record, Value};

use super::*;

// Physical byte ranges and logical starts from native-attribute-forks.json.
const FRAGMENTED_RECORDS: [(u64, usize, u32); 4] = [
	(155_894, 150, 0),
	(FIRST_CONTINUATION_RECORD, 134, FIRST_CONTINUATION),
	(156_178, 134, CONTINUATION_BLOCKS[1]),
	(156_312, 134, LAST_CONTINUATION),
];
const EXTENT_WORD_BYTES: usize = size_of::<u32>();
const EXTENT_PAIR_BYTES: usize = EXTENT_WORD_BYTES * 2;
const WIRE_EXTENTS_BYTES: usize = EXTENTS_PER_RECORD * EXTENT_PAIR_BYTES;

#[test]
fn native_fragmented_attribute_traversal_matches_every_stored_extent() {
	let source = Slices::new(IMAGE);
	let volume = Volume::open(&source).unwrap();
	let geometry = volume.header().geometry;
	let leaf = source
		.segment(LEAF_OFFSET, usize::from(NATIVE_NODE_BYTES))
		.unwrap();
	let node = Node::decode(leaf).unwrap();
	assert_eq!(node.kind(), Kind::Leaf);

	let name = units(FRAGMENTED);
	let mut expected_extents = Vec::new();
	let mut logical_start = 0;
	for (offset, length, start) in FRAGMENTED_RECORDS {
		let relative = usize::try_from(offset - LEAF_OFFSET).unwrap();
		let wire = &leaf[relative..relative + length];
		let raw = node
			.records()
			.find(|&record| std::ptr::eq(record, wire))
			.unwrap_or_else(|| panic!("no record occupies native range at byte {offset}"));
		let record = Record::decode(raw, geometry).unwrap();
		assert_eq!(record.key.file_id, OWNER);
		assert!(record.key.name.units().eq(name.iter().copied()));
		assert_eq!(record.key.start_block, start);
		assert_eq!(start, logical_start);

		let group = match record.value {
			Value::Fork(fork) if start == 0 => fork,
			Value::Extents(fork) if start != 0 => fork,
			other => panic!("unexpected record at logical block {start}: {other:?}"),
		};
		// The last eight descriptor slots in either record contain start/count pairs.
		let native_extents = wire[wire.len() - WIRE_EXTENTS_BYTES..]
			.as_chunks::<EXTENT_PAIR_BYTES>()
			.0
			.iter()
			.map(|pair| {
				let (first, count) = pair.split_at(EXTENT_WORD_BYTES);
				Extent {
					start: AllocationBlock(u32::from_be_bytes(first.try_into().unwrap())),
					blocks: u32::from_be_bytes(count.try_into().unwrap()),
				}
			})
			.filter(|extent| extent.blocks != 0);
		assert!(group.extents().eq(native_extents.clone()));
		for extent in native_extents {
			logical_start += extent.blocks;
			expected_extents.push(extent);
		}
	}
	assert_eq!(logical_start, FRAGMENTED_BLOCKS);
	assert_eq!(expected_extents.len(), FRAGMENT_COUNT);

	let attrs = volume.attributes(AttributeReadLimits::default()).unwrap();
	let value = attrs.open_attribute(OWNER, &name).unwrap().unwrap();
	let native_bytes =
		usize::from(FRAGMENTED_FULL_BLOCKS) * BLOCK_BYTES + FRAGMENTED_TRAILING_BYTES.len();
	assert_eq!(value.len(), ByteLen::from(native_bytes));
	assert_eq!(value.extents().collect::<Vec<_>>(), expected_extents);
	assert_eq!(source.payload_reads.get(), 0);
}

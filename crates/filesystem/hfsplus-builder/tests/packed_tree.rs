use hfsplus_builder::tree::{PackedTree, Record, TreeError, TreeOptions};
use hfsplus_format::btree::{
	BIG_KEYS_MASK, HFS_BTREE_TYPE, Kind, Node, NodeId, TreeHeader, TreeMetadata,
	VARIABLE_INDEX_KEYS_MASK,
};

const NATIVE_HEADER: &[u8] = include_bytes!("../../../../tests/fixtures/native-catalog-header.bin");
const NATIVE_LEAF: &[u8] = include_bytes!("../../../../tests/fixtures/native-catalog-leaf.bin");
const SMALL_NODE_BYTES: u16 = 512;
const KEY_LENGTH_BYTES: usize = size_of::<u16>();
const POINTER_BYTES: usize = size_of::<u32>();
const BITS_PER_BYTE: usize = u8::BITS as usize;
const LEAF_RECORD_BYTES: usize = 408;
const SYNTHETIC_MAX_NODES: u32 = 10_000;

fn options(max_key_bytes: u16, attributes: u32) -> TreeOptions {
	TreeOptions {
		node_bytes: SMALL_NODE_BYTES,
		max_key_bytes,
		max_nodes: SYNTHETIC_MAX_NODES,
		metadata: TreeMetadata {
			tree_type: HFS_BTREE_TYPE,
			attributes: BIG_KEYS_MASK | attributes,
			..TreeMetadata::default()
		},
	}
}

fn node(tree: &PackedTree, id: NodeId) -> Node<'_> {
	let size = usize::from(tree.header().node_bytes);
	let start = id.0 as usize * size;
	Node::decode(&tree.as_bytes()[start..start + size]).unwrap()
}

fn encoded_key_len(bytes: &[u8]) -> usize {
	usize::from(u16::from_be_bytes(*bytes.first_chunk().unwrap())) + KEY_LENGTH_BYTES
}

fn synthetic_record(ordinal: u16, key_len: u16) -> [u8; LEAF_RECORD_BYTES] {
	let mut bytes = [0; LEAF_RECORD_BYTES];
	bytes[..KEY_LENGTH_BYTES].copy_from_slice(&key_len.to_be_bytes());
	bytes[KEY_LENGTH_BYTES..KEY_LENGTH_BYTES + size_of::<u16>()]
		.copy_from_slice(&ordinal.to_be_bytes());
	bytes
}

fn descriptors(bytes: &[[u8; LEAF_RECORD_BYTES]]) -> Vec<Record<'_>> {
	bytes
		.iter()
		.map(|bytes| Record::new(bytes, encoded_key_len(bytes)))
		.collect()
}

#[test]
fn reproduces_native_single_leaf_catalog() {
	let header = TreeHeader::decode(&Node::decode(NATIVE_HEADER).unwrap(), u64::MAX).unwrap();
	let leaf = Node::decode(NATIVE_LEAF).unwrap();
	let records: Vec<_> = leaf
		.records()
		.map(|bytes| Record::new(bytes, encoded_key_len(bytes)))
		.collect();
	let packed = PackedTree::build(
		&records,
		TreeOptions {
			node_bytes: header.node_bytes,
			max_key_bytes: header.max_key_bytes,
			max_nodes: header.total_nodes,
			metadata: TreeMetadata {
				clump_bytes: header.clump_bytes,
				tree_type: header.tree_type,
				key_compare_type: header.key_compare_type,
				attributes: header.attributes,
			},
		},
	)
	.unwrap();
	assert_eq!(packed.header().depth, 1);
	assert_eq!(packed.header().root, Some(NodeId(1)));
	assert_eq!(packed.header().leaf_records, header.leaf_records);
	assert_eq!(packed.header().free_nodes, 0);
	assert_eq!(
		&packed.as_bytes()[usize::from(header.node_bytes)..],
		NATIVE_LEAF
	);
}

#[test]
fn header_only_tree_has_no_root_or_leaves() {
	let packed = PackedTree::build(&[], options(10, 0)).unwrap();
	let header_node = node(&packed, NodeId(0));
	let decoded = TreeHeader::decode(&header_node, u64::from(SMALL_NODE_BYTES)).unwrap();
	assert_eq!(decoded, *packed.header());
	assert_eq!(decoded.total_nodes, 1);
	assert_eq!(decoded.root, None);
	assert_eq!(decoded.first_leaf, None);
	assert_eq!(decoded.last_leaf, None);
	assert_eq!(header_node.forward(), None);
	assert_eq!(header_node.record(2).unwrap()[0], 0x80);
	assert!(
		header_node.record(2).unwrap()[1..]
			.iter()
			.all(|&byte| byte == 0)
	);
}

#[test]
fn rebalances_index_nodes_to_keep_two_children() {
	// Three pointer records fit in one node, but a 3+1 split would strand a child.
	let source: Vec<_> = (0..4).map(|id| synthetic_record(id, 138)).collect();
	let records = descriptors(&source);
	let packed = PackedTree::build(&records, options(138, VARIABLE_INDEX_KEYS_MASK)).unwrap();
	assert_eq!(packed.header().depth, 3);
	let root = node(&packed, packed.header().root.unwrap());
	let child_ids: Vec<_> = root.records().map(pointer).collect();
	assert_eq!(child_ids, [NodeId(5), NodeId(6)]);
	let first = node(&packed, child_ids[0]);
	let last = node(&packed, child_ids[1]);
	assert_eq!(first.records().count(), 2);
	assert_eq!(first.forward(), Some(child_ids[1]));
	assert_eq!(last.records().count(), 2);
	assert_eq!(last.backward(), Some(child_ids[0]));
	assert_eq!(
		first.records().map(pointer).collect::<Vec<_>>(),
		[NodeId(1), NodeId(2)]
	);
	assert_eq!(
		last.records().map(pointer).collect::<Vec<_>>(),
		[NodeId(3), NodeId(4)]
	);
}

#[test]
fn links_each_level_and_maps_allocated_nodes_past_the_header() {
	let header_map_bits = Node::header_map_bytes(SMALL_NODE_BYTES.into()).unwrap() * BITS_PER_BYTE;
	let leaf_count = header_map_bits + 1;
	let source: Vec<_> = (0..leaf_count)
		.map(|id| synthetic_record(u16::try_from(id).unwrap(), 2))
		.collect();
	let records = descriptors(&source);
	let options = options(2, VARIABLE_INDEX_KEYS_MASK);
	let packed = PackedTree::build(&records, options).unwrap();
	assert_eq!(
		packed.as_bytes(),
		PackedTree::build(&records, options).unwrap().as_bytes()
	);

	let mut leaf = packed.header().first_leaf;
	let mut previous = None;
	for (expected, record) in source.iter().enumerate() {
		let id = leaf.unwrap();
		let current = node(&packed, id);
		assert_eq!(current.kind(), Kind::Leaf);
		assert_eq!(current.backward(), previous);
		assert_eq!(current.records().collect::<Vec<_>>(), [record.as_slice()]);
		assert_eq!(id.0 as usize, expected + 1);
		previous = leaf;
		leaf = current.forward();
	}
	assert_eq!(leaf, None);
	assert_eq!(previous, packed.header().last_leaf);
	assert_eq!(packed.header().leaf_records as usize, leaf_count);
	let root = packed.header().root.unwrap();
	for ordinal in leaf_count + 1..=root.0 as usize {
		let id = NodeId(ordinal as u32);
		let current = node(&packed, id);
		assert_eq!(current.kind(), Kind::Index);
		assert!(current.records().count() >= 2);
		for record in current.records() {
			let child = node(&packed, pointer(record));
			assert_eq!(child.height() + 1, current.height());
			let first_record = child.record(0).unwrap();
			assert_eq!(
				&record[..encoded_key_len(record)],
				&first_record[..encoded_key_len(first_record)]
			);
		}
		if let Some(next) = current.forward() {
			let next_node = node(&packed, next);
			assert_eq!(next_node.backward(), Some(id));
			assert_eq!(next_node.height(), current.height());
		}
	}

	let header = node(&packed, NodeId(0));
	let map_id = header.forward().unwrap();
	let map = node(&packed, map_id);
	assert_eq!(map.kind(), Kind::Map);
	assert_eq!(map.backward(), None);
	assert_eq!(map.forward(), None);
	for (ordinal, byte) in header
		.record(2)
		.unwrap()
		.iter()
		.chain(map.record(0).unwrap())
		.enumerate()
	{
		for bit in 0..BITS_PER_BYTE {
			let calculated = byte & (0x80 >> bit) != 0;
			let node_id = ordinal * BITS_PER_BYTE + bit;
			assert_eq!(calculated, node_id < packed.header().total_nodes as usize);
		}
	}
	assert!(map_id.0 > root.0);
}

#[test]
fn fixed_index_slots_preserve_the_key_prefix_and_pad_before_the_pointer() {
	let source = [synthetic_record(0, 3), synthetic_record(1, 3)];
	let packed = PackedTree::build(&descriptors(&source), options(10, 0)).unwrap();
	let root = node(&packed, packed.header().root.unwrap());
	for (ordinal, record) in root.records().enumerate() {
		let key_len = encoded_key_len(record);
		assert_eq!(&record[..key_len], &source[ordinal][..key_len]);
		assert_eq!(record.len(), KEY_LENGTH_BYTES + 10 + POINTER_BYTES);
		assert!(
			record[key_len..record.len() - POINTER_BYTES]
				.iter()
				.all(|&byte| byte == 0)
		);
		assert_eq!(pointer(record).0 as usize, ordinal + 1);
	}
}

#[test]
fn rejects_malformed_records_unusable_fanout_and_node_budget() {
	let invalid = [0, 4, 0, 1, 1, 2];
	assert!(matches!(
		PackedTree::build(&[Record::new(&invalid, 4)], options(10, 0)),
		Err(TreeError::InvalidRecord(0))
	));
	let source: Vec<_> = (0..3).map(|id| synthetic_record(id, 200)).collect();
	assert!(matches!(
		PackedTree::build(
			&descriptors(&source),
			options(200, VARIABLE_INDEX_KEYS_MASK)
		),
		Err(TreeError::IndexFanout)
	));
	let one_node = TreeOptions {
		max_nodes: 1,
		..options(200, VARIABLE_INDEX_KEYS_MASK)
	};
	assert!(matches!(
		PackedTree::build(&descriptors(&source), one_node),
		Err(TreeError::NodeLimit)
	));
	let mut oversized = [0; 512];
	oversized[1] = 2;
	assert!(matches!(
		PackedTree::build(&[Record::new(&oversized, 4)], options(10, 0)),
		Err(TreeError::RecordTooLarge(0))
	));
}

fn pointer(record: &[u8]) -> NodeId {
	NodeId(u32::from_be_bytes(*record.last_chunk().unwrap()))
}

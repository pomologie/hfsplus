use hfsplus_format::{
	Error,
	btree::{
		BIG_KEYS_MASK, HEADER_USER_DATA_BYTES, HFS_BTREE_TYPE, Kind, LEAF_HEIGHT,
		METADATA_NODE_HEIGHT, MIN_INDEX_HEIGHT, Node, NodeId, NodeLinks, TreeHeader, TreeLayout,
		TreeMetadata, TreeTopology,
	},
};

const NATIVE_CATALOG_HEADER: &[u8] = include_bytes!("fixtures/native-catalog-header.bin");
const NATIVE_CATALOG_LEAF: &[u8] = include_bytes!("fixtures/native-catalog-leaf.bin");
const OPTICAL_CATALOG_LEAF: &[u8] = include_bytes!("fixtures/optical-catalog-leaf.bin");
const TREE_FORK_BYTES: u64 = 65_536;
const SMALL_NODE_BYTES: usize = 512;
const HEADER_ALLOCATED_BIT: u8 = 0x80;

#[test]
fn reconstructs_native_catalog_header_and_leaf_nodes_byte_for_byte() {
	let node = Node::decode(NATIVE_CATALOG_HEADER).unwrap();
	let native_header = TreeHeader::decode(&node, TREE_FORK_BYTES).unwrap();
	let header = TreeHeader::new(
		TreeLayout {
			node_bytes: native_header.node_bytes,
			max_key_bytes: native_header.max_key_bytes,
			total_nodes: native_header.total_nodes,
			free_nodes: native_header.free_nodes,
		},
		TreeTopology {
			depth: native_header.depth,
			root: native_header.root,
			leaf_records: native_header.leaf_records,
			first_leaf: native_header.first_leaf,
			last_leaf: native_header.last_leaf,
		},
		TreeMetadata {
			clump_bytes: native_header.clump_bytes,
			tree_type: native_header.tree_type,
			key_compare_type: native_header.key_compare_type,
			attributes: native_header.attributes,
		},
	)
	.unwrap();
	let record = header.encode_record().unwrap();
	assert_eq!(record.as_slice(), node.record(0).unwrap());
	let mut destination = vec![0xff; NATIVE_CATALOG_HEADER.len()];
	Node::encode(
		&mut destination,
		Kind::Header,
		METADATA_NODE_HEIGHT,
		NodeLinks::default(),
		&[&record, node.record(1).unwrap(), node.record(2).unwrap()],
	)
	.unwrap();
	assert_eq!(destination, NATIVE_CATALOG_HEADER);
	for fixture in [NATIVE_CATALOG_LEAF, OPTICAL_CATALOG_LEAF] {
		let node = Node::decode(fixture).unwrap();
		let records: Vec<&[u8]> = node.records().collect();
		let mut destination = vec![0xff; fixture.len()];
		let links = NodeLinks {
			forward: node.forward(),
			backward: node.backward(),
		};
		Node::encode(&mut destination, Kind::Leaf, LEAF_HEIGHT, links, &records).unwrap();
		assert_eq!(destination, fixture);
	}
}

#[test]
fn packs_index_and_bitmap_continuation_nodes() {
	let mut destination = [0xff; SMALL_NODE_BYTES];
	let keyed_pointer_a = [0, 2, 0, 1, 0, 0, 0, 8];
	let keyed_pointer_b = [0, 2, 0, 2, 0, 0, 0, 9];
	let forward = Some(NodeId(4));
	let backward = Some(NodeId(2));
	Node::encode(
		&mut destination,
		Kind::Index,
		MIN_INDEX_HEIGHT,
		NodeLinks { forward, backward },
		&[&keyed_pointer_a, &keyed_pointer_b],
	)
	.unwrap();
	let index = Node::decode(&destination).unwrap();
	assert_eq!(index.kind(), Kind::Index);
	assert_eq!(index.forward(), forward);
	assert_eq!(index.backward(), backward);
	assert!(
		index
			.records()
			.eq([keyed_pointer_a.as_slice(), keyed_pointer_b.as_slice()])
	);

	let allocation_map = vec![0xa5; Node::continuation_map_bytes(SMALL_NODE_BYTES).unwrap()];
	Node::encode(
		&mut destination,
		Kind::Map,
		METADATA_NODE_HEIGHT,
		NodeLinks {
			forward,
			backward: None,
		},
		&[&allocation_map],
	)
	.unwrap();
	let map = Node::decode(&destination).unwrap();
	assert_eq!(map.kind(), Kind::Map);
	assert_eq!(map.forward(), forward);
	assert_eq!(map.backward(), None);
	assert_eq!(map.record(0), Some(allocation_map.as_slice()));
	assert_eq!(
		destination[SMALL_NODE_BYTES - 6..SMALL_NODE_BYTES - 4],
		[0, 0]
	);
}

#[test]
fn encodes_empty_tree_and_rejects_inconsistent_topology() {
	let layout = TreeLayout {
		node_bytes: u16::try_from(SMALL_NODE_BYTES).unwrap(),
		max_key_bytes: 10,
		total_nodes: 1,
		free_nodes: 0,
	};
	let metadata = TreeMetadata {
		tree_type: HFS_BTREE_TYPE,
		attributes: BIG_KEYS_MASK,
		..TreeMetadata::default()
	};
	let header = TreeHeader::new(layout, TreeTopology::default(), metadata).unwrap();
	let mut bit_map = vec![0; Node::header_map_bytes(SMALL_NODE_BYTES).unwrap()];
	bit_map[0] = HEADER_ALLOCATED_BIT;
	let mut node_bytes = [0xff; SMALL_NODE_BYTES];
	Node::encode(
		&mut node_bytes,
		Kind::Header,
		METADATA_NODE_HEIGHT,
		NodeLinks::default(),
		&[
			&header.encode_record().unwrap(),
			&[0; HEADER_USER_DATA_BYTES],
			&bit_map,
		],
	)
	.unwrap();
	let node = Node::decode(&node_bytes).unwrap();
	assert_eq!(
		TreeHeader::decode(&node, SMALL_NODE_BYTES as u64),
		Ok(header)
	);
	assert_eq!(
		TreeHeader::new(
			layout,
			TreeTopology {
				root: Some(NodeId(0)),
				..TreeTopology::default()
			},
			metadata
		),
		Err(Error::InvalidNode)
	);
	let mut header = header;
	header.depth = u16::from(LEAF_HEIGHT);
	assert_eq!(header.encode_record(), Err(Error::InvalidNode));
}

#[test]
fn rejects_invalid_node_geometry_and_records_before_writing() {
	let fill = [0xa5; SMALL_NODE_BYTES];
	let mut destination = fill;
	let links = NodeLinks::default();
	let valid_record = [0; 2];
	let record_bytes = Node::record_capacity(SMALL_NODE_BYTES, 1).unwrap();
	let oversized = vec![0; record_bytes + 2];
	let malformed = [
		(Kind::Leaf, LEAF_HEIGHT, links, oversized.as_slice()),
		(Kind::Leaf, LEAF_HEIGHT, links, &[0][..]),
		(Kind::Leaf, LEAF_HEIGHT, links, &[][..]),
		(Kind::Leaf, METADATA_NODE_HEIGHT, links, &valid_record[..]),
		(
			Kind::Leaf,
			LEAF_HEIGHT,
			NodeLinks {
				forward: Some(NodeId(0)),
				backward: None,
			},
			&valid_record[..],
		),
		(Kind::Index, MIN_INDEX_HEIGHT, links, &valid_record[..]),
		(Kind::Map, METADATA_NODE_HEIGHT, links, &valid_record[..]),
	];
	for (kind, height, links, record) in malformed {
		assert_eq!(
			Node::encode(&mut destination, kind, height, links, &[record]),
			Err(Error::InvalidNode)
		);
		assert_eq!(destination, fill);
	}
	assert_eq!(
		Node::record_capacity(SMALL_NODE_BYTES, usize::MAX),
		Err(Error::InvalidNode)
	);
	assert_eq!(
		Node::record_capacity(SMALL_NODE_BYTES + 1, 1),
		Err(Error::InvalidNode)
	);
}

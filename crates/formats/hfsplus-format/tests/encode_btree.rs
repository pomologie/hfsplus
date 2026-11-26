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
fn exact_single_record_capacity_matches_the_wire_layout() {
	// TN1150 defines a 14-byte descriptor followed by the records. The
	// trailing two u16 offsets describe the free space and the first record.
	const LEAF_DESCRIPTOR: [u8; 14] = [0, 0, 0, 0, 0, 0, 0, 0, 0xff, 1, 0, 1, 0, 0];
	const OFFSET_TABLE_BYTES: usize = 4;
	const LEAF_RECORD_COUNT: usize = 1;
	const RECORD_BYTE: u8 = 0xa5;
	const SENTINEL: u8 = 0x5a;

	for node_bytes in [512, 1024, 2048, 4096, 8192, 16_384, 32_768] {
		let table_start = node_bytes - OFFSET_TABLE_BYTES;
		let capacity = table_start - LEAF_DESCRIPTOR.len();
		let expected_offsets = [
			u16::try_from(table_start).unwrap().to_be_bytes(),
			u16::try_from(LEAF_DESCRIPTOR.len()).unwrap().to_be_bytes(),
		]
		.concat();
		assert_eq!(
			Node::record_capacity(node_bytes, LEAF_RECORD_COUNT),
			Ok(capacity)
		);

		let data = vec![RECORD_BYTE; capacity + 2];
		let mut destination = vec![SENTINEL; node_bytes];
		let exact_record = &data[..capacity];
		Node::encode(
			&mut destination,
			Kind::Leaf,
			LEAF_HEIGHT,
			NodeLinks::default(),
			&[exact_record],
		)
		.unwrap();

		assert_eq!(&destination[..LEAF_DESCRIPTOR.len()], &LEAF_DESCRIPTOR);
		assert_eq!(
			&destination[LEAF_DESCRIPTOR.len()..table_start],
			exact_record
		);
		assert_eq!(&destination[table_start..], &expected_offsets);
		assert_eq!(
			Node::decode(&destination).unwrap().record(0),
			Some(exact_record)
		);

		let oversized = [&data[..capacity + 1], data.as_slice()];
		for record in oversized {
			destination.fill(SENTINEL);
			assert_eq!(
				Node::encode(
					&mut destination,
					Kind::Leaf,
					LEAF_HEIGHT,
					NodeLinks::default(),
					&[record],
				),
				Err(Error::InvalidNode)
			);
			assert!(destination.iter().all(|byte| *byte == SENTINEL));
		}
	}
}

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
fn tree_header_requests_require_a_header_node_and_the_full_fork() {
	let catalog_header = Node::decode(NATIVE_CATALOG_HEADER).unwrap();
	let header = TreeHeader::decode(&catalog_header, TREE_FORK_BYTES).unwrap();
	assert_eq!(
		TreeHeader::node_size_hint(NATIVE_CATALOG_HEADER),
		Ok(header.node_bytes),
	);
	assert_eq!(
		TreeHeader::decode(&catalog_header, TREE_FORK_BYTES - 1),
		Err(Error::InvalidNode),
	);

	let catalog_leaf = Node::decode(NATIVE_CATALOG_LEAF).unwrap();
	assert_eq!(
		TreeHeader::decode(&catalog_leaf, TREE_FORK_BYTES),
		Err(Error::InvalidNode),
	);
	assert_eq!(
		TreeHeader::node_size_hint(NATIVE_CATALOG_LEAF),
		Err(Error::InvalidNode),
	);
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
	for unsupported_size in [SMALL_NODE_BYTES - 1, SMALL_NODE_BYTES + 1] {
		assert_eq!(
			Node::record_capacity(unsupported_size, 1),
			Err(Error::InvalidNode)
		);
	}
}

#[test]
fn offset_table_budget_includes_the_descriptor_even_without_record_bytes() {
	const RECORDS_AT_DESCRIPTOR_BOUNDARY: usize = 248;
	const RECORDS_BEYOND_NODE: usize = 256;

	assert_eq!(
		Node::record_capacity(SMALL_NODE_BYTES, RECORDS_AT_DESCRIPTOR_BOUNDARY - 1),
		Ok(size_of::<u16>()),
	);
	assert_eq!(
		Node::record_capacity(SMALL_NODE_BYTES, RECORDS_AT_DESCRIPTOR_BOUNDARY),
		Ok(0),
	);
	for count in [RECORDS_AT_DESCRIPTOR_BOUNDARY + 1, RECORDS_BEYOND_NODE] {
		assert_eq!(
			Node::record_capacity(SMALL_NODE_BYTES, count),
			Err(Error::InvalidNode),
		);
	}
}

#[test]
fn typed_header_rejects_layout_and_topology_outside_its_node_budget() {
	const FORK_NODES: u32 = 3;
	const FIRST_LEAF: NodeId = NodeId(1);
	const NODE_WIDTH: u16 = SMALL_NODE_BYTES as u16;
	const TOO_MANY_LEVELS: u16 = u8::MAX as u16 + 1;

	let layout = TreeLayout {
		node_bytes: NODE_WIDTH,
		max_key_bytes: 10,
		total_nodes: FORK_NODES,
		free_nodes: 1,
	};
	let topology = TreeTopology {
		depth: u16::from(LEAF_HEIGHT),
		root: Some(FIRST_LEAF),
		leaf_records: 1,
		first_leaf: Some(FIRST_LEAF),
		last_leaf: Some(FIRST_LEAF),
	};
	let metadata = TreeMetadata::default();
	let header = TreeHeader::new(layout, topology, metadata).unwrap();
	assert_eq!(header.free_nodes, 1);
	assert!(header.encode_record().is_ok());

	for rejected_layout in [
		TreeLayout {
			node_bytes: NODE_WIDTH + 1,
			..layout
		},
		TreeLayout {
			max_key_bytes: NODE_WIDTH,
			..layout
		},
		TreeLayout {
			free_nodes: FORK_NODES,
			..layout
		},
	] {
		assert_eq!(
			TreeHeader::new(rejected_layout, topology, metadata),
			Err(Error::InvalidNode),
		);
	}
	for rejected_topology in [
		TreeTopology {
			depth: TOO_MANY_LEVELS,
			..topology
		},
		TreeTopology {
			last_leaf: Some(NodeId(FORK_NODES)),
			..topology
		},
		TreeTopology {
			root: None,
			..topology
		},
		TreeTopology {
			leaf_records: 0,
			..topology
		},
		TreeTopology {
			leaf_records: 1,
			..TreeTopology::default()
		},
	] {
		assert_eq!(
			TreeHeader::new(layout, rejected_topology, metadata),
			Err(Error::InvalidNode),
		);
	}
}

#[test]
fn typed_node_admission_refuses_inconsistent_levels_and_header_record_count() {
	const SENTINEL: u8 = 0xa5;
	let native_header = Node::decode(NATIVE_CATALOG_HEADER).unwrap();
	let header_records: Vec<_> = native_header.records().collect();
	let index_records: [&[u8]; 2] = [&[0, 2, 0, 1, 0, 0, 0, 8], &[0, 2, 0, 2, 0, 0, 0, 9]];
	let map_bytes = vec![0; Node::continuation_map_bytes(NATIVE_CATALOG_HEADER.len()).unwrap()];
	let map_records: [&[u8]; 1] = [&map_bytes];
	let mut destination = vec![SENTINEL; NATIVE_CATALOG_HEADER.len()];
	for (kind, height, records) in [
		(Kind::Index, LEAF_HEIGHT, index_records.as_slice()),
		(Kind::Map, LEAF_HEIGHT, map_records.as_slice()),
		(Kind::Header, LEAF_HEIGHT, header_records.as_slice()),
		(Kind::Header, METADATA_NODE_HEIGHT, &header_records[..2]),
	] {
		assert_eq!(
			Node::encode(
				&mut destination,
				kind,
				height,
				NodeLinks::default(),
				records
			),
			Err(Error::InvalidNode),
		);
		assert!(destination.iter().all(|&byte| byte == SENTINEL));
	}
}

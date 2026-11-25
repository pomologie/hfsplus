use super::*;

#[test]
fn proves_an_empty_tree_and_refuses_hidden_allocated_nodes() {
	let (mut image, _) = volume_with_tree(VOLUME_BLOCKS, TREE_NODES);
	for (allocated, first_map, expected) in [
		(0x80, None, true),
		(0xc0, None, false),
		(0xc0, Some(ROOT), false),
	] {
		let free_nodes = TREE_NODES - u8::count_ones(allocated);
		let header = empty_header(TREE_NODES, free_nodes);
		let mut map = vec![0; Node::header_map_bytes(BLOCK_BYTES).unwrap()];
		map[0] = allocated;
		Node::encode(
			node_bytes(&mut image, NodeId(0)),
			Kind::Header,
			METADATA_NODE_HEIGHT,
			NodeLinks {
				forward: first_map,
				backward: None,
			},
			&[
				&header.encode_record().unwrap(),
				&[0; HEADER_USER_DATA_BYTES],
				&map,
			],
		)
		.unwrap();
		let volume = Volume::open(image.as_slice()).unwrap();
		let result = volume.overflow_tree_is_empty(OverflowLimits::default());
		if expected {
			assert!(result.unwrap());
			let limited = OverflowLimits {
				max_metadata_reads: 1,
				..OverflowLimits::default()
			};
			assert!(matches!(
				volume.overflow_tree_is_empty(limited),
				Err(OverflowError::Limit(OverflowLimit::MetadataReads))
			));
		} else {
			assert!(matches!(
				result,
				Err(OverflowError::Read(Error::Format(FormatError::InvalidNode)))
			));
		}
	}
	write_tree(&mut image);
	assert!(
		!Volume::open(image.as_slice())
			.unwrap()
			.overflow_tree_is_empty(OverflowLimits::default())
			.unwrap()
	);
}

#[test]
fn counts_the_empty_trees_allocation_map_continuation() {
	let map_bytes = Node::header_map_bytes(BLOCK_BYTES).unwrap();
	let tree_nodes = (map_bytes as u32) * u8::BITS + 1;
	let (mut image, _) = volume_with_tree(tree_nodes + VOLUME_BLOCKS, tree_nodes);
	let header = empty_header(tree_nodes, tree_nodes - 2);
	let mut map = vec![0; map_bytes];
	map[0] = 0xc0;
	Node::encode(
		node_bytes(&mut image, NodeId(0)),
		Kind::Header,
		METADATA_NODE_HEIGHT,
		NodeLinks {
			forward: Some(ROOT),
			backward: None,
		},
		&[
			&header.encode_record().unwrap(),
			&[0; HEADER_USER_DATA_BYTES],
			&map,
		],
	)
	.unwrap();
	let continuation = vec![0; Node::continuation_map_bytes(BLOCK_BYTES).unwrap()];
	Node::encode(
		node_bytes(&mut image, ROOT),
		Kind::Map,
		METADATA_NODE_HEIGHT,
		NodeLinks::default(),
		&[&continuation],
	)
	.unwrap();
	let volume = Volume::open(image.as_slice()).unwrap();
	assert!(
		volume
			.overflow_tree_is_empty(OverflowLimits::default())
			.unwrap()
	);
	let limited = OverflowLimits {
		max_metadata_reads: 2,
		..OverflowLimits::default()
	};
	assert!(matches!(
		volume.overflow_tree_is_empty(limited),
		Err(OverflowError::Limit(OverflowLimit::MetadataReads))
	));
}

fn empty_header(total_nodes: u32, free_nodes: u32) -> TreeHeader {
	TreeHeader::new(
		TreeLayout {
			node_bytes: BLOCK_BYTES as u16,
			max_key_bytes: KEY_LENGTH,
			total_nodes,
			free_nodes,
		},
		TreeTopology::default(),
		TreeMetadata {
			attributes: BIG_KEYS_MASK,
			..TreeMetadata::default()
		},
	)
	.unwrap()
}

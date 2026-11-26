use hfsplus_format::{
	Error,
	btree::{Kind, LEAF_HEIGHT, MIN_INDEX_HEIGHT, Node, NodeLinks},
	extents::{ForkKind, KEY_BYTES, Key},
	volume::{AllocationBlock, CatalogId, Extent},
};

#[test]
fn encodes_the_fixed_key_and_zero_pads_unused_extent_slots() {
	let key = Key {
		id: CatalogId(0x0102_0304),
		kind: ForkKind::Resource,
		start_block: 0x0506_0708,
	};
	let extent = Extent {
		start: AllocationBlock(0x0a0b_0c0d),
		blocks: 3,
	};
	let record = key.encode_record(&[extent]).unwrap();
	let used_prefix = [
		0, 10, 0xff, 0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 11, 12, 13, 0, 0, 0, 3,
	];
	assert_eq!(record[..used_prefix.len()], used_prefix);
	assert!(record[used_prefix.len()..].iter().all(|&byte| byte == 0));
	assert_eq!(Key::decode(&record).unwrap(), key);
	assert!(matches!(
		Key::decode(&record[..KEY_BYTES - 1]),
		Err(Error::InvalidNode)
	));
	assert!(matches!(key.encode_record(&[]), Err(Error::InvalidFork)));
}

#[test]
fn matches_ordered_extent_nodes_against_independent_wire_records() {
	const NODE_SIZES: [usize; 7] = [512, 1024, 2048, 4096, 8192, 16_384, 32_768];
	const CATALOG_IDS: [u32; 2] = [17, 18];
	const LOGICAL_STARTS: [u32; 2] = [8, 16];
	const FORKS: [(ForkKind, u8); 2] = [(ForkKind::Data, 0), (ForkKind::Resource, 0xff)];
	const WIRE_EXTENT_KEY_LENGTH: u16 = 10;
	const LEAF_RECORD_BYTES: usize = 76;
	const FIRST_PHYSICAL_BLOCK: u32 = 100;
	const EXTENT_BLOCKS: u32 = 8;
	const EXPECTED_CASES: usize = 103;
	let entries: Vec<_> = CATALOG_IDS
		.into_iter()
		.flat_map(|id| {
			FORKS.into_iter().flat_map(move |(kind, wire_kind)| {
				LOGICAL_STARTS.into_iter().map(move |start_block| {
					(
						Key {
							id: CatalogId(id),
							kind,
							start_block,
						},
						wire_kind,
					)
				})
			})
		})
		.collect();
	let mut cases = 0;
	for (kind, wire_kind, height, minimum_records) in [
		(Kind::Leaf, 0xff, LEAF_HEIGHT, 1),
		(Kind::Index, 0, MIN_INDEX_HEIGHT, 2),
	] {
		let records: Vec<_> = entries
			.iter()
			.enumerate()
			.map(|(ordinal, &(key, fork))| {
				let mut wire = Vec::from(WIRE_EXTENT_KEY_LENGTH.to_be_bytes());
				wire.extend([fork, 0]);
				wire.extend(key.id.0.to_be_bytes());
				wire.extend(key.start_block.to_be_bytes());
				if kind == Kind::Leaf {
					let extent = Extent {
						start: AllocationBlock(
							FIRST_PHYSICAL_BLOCK + ordinal as u32 * EXTENT_BLOCKS,
						),
						blocks: EXTENT_BLOCKS,
					};
					wire.extend(extent.start.0.to_be_bytes());
					wire.extend(extent.blocks.to_be_bytes());
					wire.resize(LEAF_RECORD_BYTES, 0);
					assert_eq!(key.encode_record(&[extent]).unwrap().as_slice(), wire);
				} else {
					let child = ordinal as u32 + 1;
					wire.extend(child.to_be_bytes());
				}
				wire
			})
			.collect();
		for size in NODE_SIZES {
			for count in minimum_records..=entries.len() {
				let records = &records[..count];
				let Some(expected) = wire_node(size, wire_kind, height, records) else {
					continue;
				};
				let mut encoded = vec![u8::MAX; size];
				let borrowed: Vec<_> = records.iter().map(Vec::as_slice).collect();
				Node::encode(&mut encoded, kind, height, NodeLinks::default(), &borrowed).unwrap();
				assert_eq!(encoded, expected);
				let node = Node::decode(&expected).unwrap();
				assert_eq!((node.kind(), node.height()), (kind, height));
				assert_eq!(node.forward(), None);
				assert_eq!(node.backward(), None);
				assert_eq!(node.record(count), None);
				assert!(node.records().eq(borrowed));
				let keys: Vec<_> = node
					.records()
					.map(|record| Key::decode(record).unwrap())
					.collect();
				assert!(
					keys.iter()
						.copied()
						.eq(entries[..count].iter().map(|entry| entry.0))
				);
				assert!(keys.is_sorted());
				cases += 1;
			}
		}
	}
	assert_eq!(cases, EXPECTED_CASES);
	println!("Completed {cases} ordered extent node cases");
}

fn wire_node(size: usize, kind: u8, height: u8, records: &[Vec<u8>]) -> Option<Vec<u8>> {
	const LINK_BYTES: usize = 8;
	const OFFSET_BYTES: usize = size_of::<u16>();
	let mut node = vec![0; LINK_BYTES];
	node.extend([kind, height]);
	node.extend(u16::try_from(records.len()).unwrap().to_be_bytes());
	node.extend([0; 2]);
	let table_start = size - (records.len() + 1) * OFFSET_BYTES;
	let mut offsets = Vec::with_capacity(records.len() + 1);
	for record in records {
		offsets.push(node.len());
		node.extend(record);
	}
	if node.len() > table_start {
		return None;
	}
	offsets.push(node.len());
	node.resize(table_start, 0);
	for offset in offsets.into_iter().rev() {
		node.extend(u16::try_from(offset).unwrap().to_be_bytes());
	}
	Some(node)
}

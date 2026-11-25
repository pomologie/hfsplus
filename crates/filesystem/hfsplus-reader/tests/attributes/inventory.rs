use hfsplus_format::{
	attributes::InlineRecord,
	btree::{LEAF_HEIGHT, Node, NodeLinks, TreeHeader},
};

use super::*;

#[test]
fn inventories_all_inline_values_and_reports_owners_verbatim() {
	let fixture = inline_fixture();
	let volume = Volume::open(SparseVolume::new(&fixture)).unwrap();
	let attributes = volume.attributes(AttributeReadLimits::default()).unwrap();
	let mut entries = Vec::new();
	attributes
		.visit_inline_attrs::<AttributeReadError<_>>(|owner, name, value| {
			let units = name.units().collect::<Vec<_>>();
			entries.push((owner, String::from_utf16(&units).unwrap(), value.to_vec()));
			Ok(())
		})
		.unwrap();
	assert_eq!(entries.len(), 8);
	assert_eq!(entries[1], (OWNER, EMPTY.to_owned(), vec![]));
	assert_eq!(entries[2], (OWNER, DECOMPOSED.to_owned(), vec![2]));
	assert_eq!(
		entries[4].2,
		vec![INLINE_FILL_BYTE; NATIVE_INLINE_MAX_VALUE_BYTES]
	);
	assert_eq!(entries[7], (ABSENT_OWNER, COMPOSED.to_owned(), vec![1]));
	for limits in [
		AttributeReadLimits {
			max_nodes: 2,
			..AttributeReadLimits::default()
		},
		AttributeReadLimits {
			max_entries: entries.len() - 1,
			..AttributeReadLimits::default()
		},
	] {
		let attributes = volume.attributes(limits).unwrap();
		let result = attributes.visit_inline_attrs::<AttributeReadError<_>>(|_, _, _| Ok(()));
		assert!(matches!(
			result,
			Err(AttributeReadError::NodeLimit | AttributeReadError::EntryLimit)
		));
	}
}

#[test]
fn refuses_a_native_fork_backed_attribute() {
	let volume = Volume::open(SparseVolume::new(NATIVE)).unwrap();
	let attributes = volume.attributes(AttributeReadLimits::default()).unwrap();
	assert!(matches!(
		attributes.visit_inline_attrs::<AttributeReadError<_>>(|_, _, _| Ok(())),
		Err(AttributeReadError::NonInline { .. })
	));
}

#[test]
fn refuses_an_allocated_node_outside_the_rooted_graph() {
	let mut fixture = inline_fixture();
	let header_node = Node::decode(&fixture[fixture_offset(NodeId(0))..][..NODE_BYTES]).unwrap();
	let original = Volume::open(SparseVolume::new(&fixture)).unwrap();
	let mut header = TreeHeader::decode(
		&header_node,
		original.header().forks.attributes.logical_bytes,
	)
	.unwrap();
	let user = header_node.record(1).unwrap().to_vec();
	let mut map = header_node.record(2).unwrap().to_vec();
	const FIRST_FREE_NODE: u32 = 4;
	let free_node_mask = 0x80 >> FIRST_FREE_NODE;
	assert_eq!(map[0] & free_node_mask, 0);
	map[0] |= free_node_mask;
	// Even a corresponding free-node count cannot account for the extra node.
	header.free_nodes -= 1;
	Node::encode(
		&mut fixture[fixture_offset(NodeId(0))..][..NODE_BYTES],
		hfsplus_format::btree::Kind::Header,
		hfsplus_format::btree::METADATA_NODE_HEIGHT,
		NodeLinks::default(),
		&[&header.encode_record().unwrap(), &user, &map],
	)
	.unwrap();
	let volume = Volume::open(SparseVolume::new(&fixture)).unwrap();
	let attributes = volume.attributes(AttributeReadLimits::default()).unwrap();
	assert!(matches!(
		attributes.visit_inline_attrs::<AttributeReadError<_>>(|_, _, _| Ok(())),
		Err(AttributeReadError::Format {
			error: FormatError::InvalidNode,
			..
		})
	));
}

fn inline_fixture() -> Vec<u8> {
	let mut fixture = NATIVE.to_vec();
	for id in [FIRST_LEAF, LAST_LEAF] {
		let offset = fixture_offset(id);
		let leaf = Node::decode(&fixture[offset..][..NODE_BYTES]).unwrap();
		let links = NodeLinks {
			forward: leaf.forward(),
			backward: leaf.backward(),
		};
		let records = leaf
			.records()
			.map(|raw| {
				let (key, _) = Key::split(raw).unwrap();
				let units = key.name.units().collect::<Vec<_>>();
				let name = String::from_utf16(&units).unwrap();
				let (owner, bytes): (CatalogId, &[u8]) = match name.as_str() {
					FIRST_FORK | SECOND_FORK => (key.file_id, b"replacement"),
					COMPOSED => (ABSENT_OWNER, &[1]),
					_ => return raw.to_vec(),
				};
				let mut result = vec![0; InlineRecord::encoded_len(&units, bytes).unwrap()];
				InlineRecord::encode(owner, &units, bytes, &mut result).unwrap();
				result
			})
			.collect::<Vec<_>>();
		let references = records.iter().map(Vec::as_slice).collect::<Vec<_>>();
		Node::encode(
			&mut fixture[offset..][..NODE_BYTES],
			hfsplus_format::btree::Kind::Leaf,
			LEAF_HEIGHT,
			links,
			&references,
		)
		.unwrap();
	}
	fixture
}

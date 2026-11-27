use crate::binary::put;

use super::{
	Error, Kind, LEAF_HEIGHT, METADATA_NODE_HEIGHT, MIN_NODE_BYTES, Node, NodeLinks,
	RECORD_ALIGNMENT_BYTES, TreeHeader, descriptor, header_record, offset_table_start,
	record_offset_position,
};

#[test]
fn rejects_inconsistent_leaf_descriptor_and_record_boundaries() {
	const RECORD: [u8; RECORD_ALIGNMENT_BYTES] = [0; RECORD_ALIGNMENT_BYTES];
	let records = [&RECORD[..]];
	let mut leaf = [0; MIN_NODE_BYTES];
	Node::encode(
		&mut leaf,
		Kind::Leaf,
		LEAF_HEIGHT,
		NodeLinks::default(),
		&records,
	)
	.unwrap();
	assert_eq!(Node::decode(&leaf).unwrap().record(0), Some(records[0]));

	let mut wrong_height = leaf;
	wrong_height[descriptor::HEIGHT] = METADATA_NODE_HEIGHT;
	assert!(matches!(
		Node::decode(&wrong_height),
		Err(Error::InvalidNode)
	));

	let table_start = offset_table_start(leaf.len(), records.len()).unwrap();
	let record_end = descriptor::BYTES + RECORD.len();
	for (ordinal, offset) in [
		(0, record_end),
		(1, table_start + RECORD_ALIGNMENT_BYTES),
		(1, record_end + 1),
	] {
		let mut inconsistent = leaf;
		put(
			&mut inconsistent,
			record_offset_position(leaf.len(), ordinal),
			u16::try_from(offset).unwrap().to_be_bytes(),
		)
		.unwrap();
		assert!(matches!(
			Node::decode(&inconsistent),
			Err(Error::InvalidNode)
		));
	}
}

#[test]
fn rejects_a_header_size_hint_between_supported_widths() {
	const SIZE_OFFSET: usize = descriptor::BYTES + header_record::NODE_BYTES;
	const PREFIX_BYTES: usize = SIZE_OFFSET + size_of::<u16>();
	const SUPPORTED_WIDTH: u16 = MIN_NODE_BYTES as u16;
	let mut prefix = [0; PREFIX_BYTES];
	prefix[descriptor::KIND] = descriptor::HEADER_KIND;
	put(&mut prefix, SIZE_OFFSET, SUPPORTED_WIDTH.to_be_bytes()).unwrap();
	assert_eq!(TreeHeader::node_size_hint(&prefix), Ok(SUPPORTED_WIDTH));

	put(
		&mut prefix,
		SIZE_OFFSET,
		(SUPPORTED_WIDTH + 1).to_be_bytes(),
	)
	.unwrap();
	assert_eq!(TreeHeader::node_size_hint(&prefix), Err(Error::InvalidNode));
}

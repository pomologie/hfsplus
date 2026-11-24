use hfsplus_format::{
	Error,
	btree::{Kind, Node},
	volume::{AllocationBlock, Header, Signature},
};

const PRIMARY: &[u8] = include_bytes!("fixtures/native-primary.bin");
const ALTERNATE: &[u8] = include_bytes!("fixtures/native-alternate.bin");
const CATALOG_HEADER: &[u8] = include_bytes!("fixtures/native-catalog-header.bin");
const CATALOG_LEAF: &[u8] = include_bytes!("fixtures/native-catalog-leaf.bin");
const RAW_SIZE: u64 = 8 * 1024 * 1024;

#[test]
fn decodes_native_primary_and_alternate_headers() {
	let header = Header::decode(PRIMARY, RAW_SIZE).unwrap();
	assert_eq!(header, Header::decode(ALTERNATE, RAW_SIZE).unwrap());
	assert_eq!(header.signature, Signature::HfsPlus);
	assert_eq!(header.geometry.block_size(), 4096);
	assert_eq!(header.geometry.total_blocks(), 2048);
	assert_eq!(header.geometry.alternate_header_offset(), RAW_SIZE - 1024);
	assert!(header.unmounted_cleanly());
	assert_eq!(header.journal_info, None);
	assert!(!header.forks.catalog.has_overflow());
	let extent = header.forks.catalog.extents().next().unwrap();
	assert_eq!(
		header
			.geometry
			.block_range(extent.start, extent.blocks)
			.unwrap()
			.start,
		794_624
	);
}

#[test]
fn decodes_native_catalog_node_records() {
	let header = Node::decode(CATALOG_HEADER).unwrap();
	assert_eq!(header.kind(), Kind::Header);
	assert_eq!(header.records().count(), 3);
	assert_eq!(header.record(0).unwrap().len(), 106);
	let leaf = Node::decode(CATALOG_LEAF).unwrap();
	assert_eq!(leaf.kind(), Kind::Leaf);
	assert_eq!(leaf.height(), 1);
	assert_eq!(leaf.records().count(), 2);
	assert_eq!(leaf.forward(), None);
	assert!(leaf.record(2).is_none());
}

#[test]
fn rejects_geometry_and_extent_escape_and_reverse_record_offsets() {
	assert_eq!(
		Header::decode(&PRIMARY[..511], RAW_SIZE),
		Err(Error::Truncated)
	);
	assert_eq!(
		Header::decode(PRIMARY, RAW_SIZE - 1),
		Err(Error::InvalidGeometry)
	);
	let header = Header::decode(PRIMARY, RAW_SIZE).unwrap();
	assert_eq!(
		header.geometry.block_range(AllocationBlock(u32::MAX), 2),
		Err(Error::InvalidGeometry)
	);
	let mut invalid = PRIMARY.to_vec();
	invalid[288..292].copy_from_slice(&2048_u32.to_be_bytes());
	assert_eq!(Header::decode(&invalid, RAW_SIZE), Err(Error::InvalidFork));
	let mut invalid = CATALOG_LEAF.to_vec();
	let second = invalid.len() - 4;
	invalid[second..second + 2].copy_from_slice(&14_u16.to_be_bytes());
	assert!(matches!(Node::decode(&invalid), Err(Error::InvalidNode)));
}

#[test]
fn reads_native_tree_header_and_matches_root_thread_to_directory_key() {
	use hfsplus_format::{
		btree::{NodeId, TreeHeader},
		catalog::{Record, ThreadKind, Value},
		volume::CatalogId,
	};

	let header = Header::decode(PRIMARY, RAW_SIZE).unwrap();
	let tree = TreeHeader::decode(
		&Node::decode(CATALOG_HEADER).unwrap(),
		header.forks.catalog.logical_bytes,
	)
	.unwrap();
	assert_eq!(tree.root, Some(NodeId(1)));
	assert_eq!(tree.leaf_records, 2);
	assert_eq!(TreeHeader::node_size_hint(&CATALOG_HEADER[..34]), Ok(4096));
	let leaf = Node::decode(CATALOG_LEAF).unwrap();
	let root = Record::decode(leaf.record(0).unwrap(), header.geometry).unwrap();
	let thread = Record::decode(leaf.record(1).unwrap(), header.geometry).unwrap();
	let Value::Folder {
		metadata, valence, ..
	} = root.value
	else {
		panic!("expected root folder")
	};
	let Value::Thread { parent, name, kind } = thread.value else {
		panic!("expected root thread")
	};
	assert_eq!(metadata.id, CatalogId(2));
	assert_eq!(metadata.permissions.mode, 0o40755);
	assert_eq!(valence, 0);
	assert_eq!(kind, ThreadKind::Folder);
	assert_eq!(thread.key.parent_or_thread, metadata.id);
	assert_eq!(parent, root.key.parent_or_thread);
	assert!(name.units().eq("Pomologie Fixture".encode_utf16()));
	assert_eq!(name, root.key.name);
}

#[test]
fn reads_native_regular_file_and_matches_its_thread() {
	use hfsplus_format::catalog::{Record, ThreadKind, Value};

	let header = Header::decode(include_bytes!("fixtures/optical-primary.bin"), 20_480).unwrap();
	let leaf = Node::decode(include_bytes!("fixtures/optical-catalog-leaf.bin")).unwrap();
	let records: Vec<_> = leaf
		.records()
		.map(|raw| Record::decode(raw, header.geometry).unwrap())
		.collect();
	let file = records
		.iter()
		.find(|record| record.key.name.units().eq("hello.txt".encode_utf16()))
		.unwrap();
	let Value::File {
		metadata,
		data,
		resource,
	} = file.value
	else {
		panic!("expected file")
	};
	assert_eq!(data.logical_bytes, 18);
	assert_eq!(resource.logical_bytes, 0);
	assert_eq!(data.extents().count(), 1);
	let thread = records
		.iter()
		.find(|record| record.key.parent_or_thread == metadata.id && record.key.name.is_empty())
		.unwrap();
	assert!(
		matches!(thread.value, Value::Thread { kind: ThreadKind::File, name, .. } if name == file.key.name)
	);
}

use core::{num::NonZeroU32, ops::Range};

use hfsplus_format::{
	attributes::InlineRecord,
	catalog::{Key, Record, ThreadKind, Value},
	hard_links::{
		Error, Inode, LinkReference, Neighbors, PRIVATE_DIRECTORY_NAME, Stub,
		is_private_directory_key, private_directory_value,
	},
	volume::{CatalogId, Geometry},
};

const RECORDS: &[u8] = include_bytes!("fixtures/native-hard-links.bin");
const PRIVATE_DIRECTORY_ID: CatalogId = CatalogId(16);
const ROOT_ID: CatalogId = CatalogId(2);
const ROOT_CREATED: u32 = 3_878_405_759;
const BLOCK_BYTES: u32 = 4096;
const BLOCKS: u32 = 4096;
const VOLUME_BYTES: u64 = 16 << 20;
const PRIVATE_DIRECTORY: Range<usize> = 0..138;
const ONE: Range<usize> = 138..400;
const TWO: Range<usize> = 400..662;
const THREE: Range<usize> = 662..928;
const FIRST_INODE: Range<usize> = 928..1198;
const FOUR: Range<usize> = 1198..1462;
const FIVE: Range<usize> = 1462..1726;
const SECOND_INODE: Range<usize> = 1726..1996;
const INODE_ATTRIBUTE: Range<usize> = 2248..2340;
const LEGACY_ONE: Range<usize> = 2340..2602;
const LEGACY_INODE: Range<usize> = 2602..2874;

fn geometry() -> Geometry {
	Geometry::new(BLOCK_BYTES, BLOCKS, VOLUME_BYTES).unwrap()
}

#[test]
fn reproduces_native_private_directory_and_matching_threads() {
	let native = &RECORDS[PRIVATE_DIRECTORY];
	let record = Record::decode(native, geometry()).unwrap();
	assert!(is_private_directory_key(record.key));
	assert!(record.key.name.units().eq(PRIVATE_DIRECTORY_NAME));
	let value = private_directory_value(PRIVATE_DIRECTORY_ID, ROOT_CREATED, 2).unwrap();
	let mut bytes = vec![0xa5; native.len() + 1];
	let used = Record::encode(
		ROOT_ID,
		&PRIVATE_DIRECTORY_NAME,
		value,
		geometry(),
		&mut bytes,
	)
	.unwrap();
	assert_eq!(&bytes[..used], native);
	assert_eq!(bytes[used], 0xa5);

	let threads = [
		(
			1996..2056,
			16,
			2,
			"\0\0\0\0HFS+ Private Data",
			ThreadKind::Folder,
		),
		(2056..2088, 19, 16, "iNode19", ThreadKind::File),
		(2088..2112, 20, 2, "one", ThreadKind::File),
		(2112..2136, 21, 2, "two", ThreadKind::File),
		(2136..2164, 22, 18, "three", ThreadKind::File),
		(2164..2196, 23, 16, "iNode23", ThreadKind::File),
		(2196..2222, 24, 2, "four", ThreadKind::File),
		(2222..2248, 25, 18, "five", ThreadKind::File),
	];
	for (span, id, original_parent, original_name, thread_kind) in threads {
		let record = Record::decode(&RECORDS[span], geometry()).unwrap();
		assert_eq!(record.key.parent_or_thread, CatalogId(id));
		assert!(record.key.name.is_empty());
		let Value::Thread { kind, parent, name } = record.value else {
			panic!("missing native thread");
		};
		assert_eq!(kind, thread_kind);
		assert_eq!(parent, CatalogId(original_parent));
		assert!(name.units().eq(original_name.encode_utf16()));
	}
}

#[test]
fn round_trips_the_native_stubs_and_inodes_byte_for_byte() {
	let stubs = [
		(ONE, 20, 19, None, Some(21)),
		(TWO, 21, 19, Some(20), Some(22)),
		(THREE, 22, 19, Some(21), None),
		(FOUR, 24, 23, None, Some(25)),
		(FIVE, 25, 23, Some(24), None),
	];
	for (span, cnid, reference, next, previous) in stubs {
		let native = &RECORDS[span];
		let record = Record::decode(native, geometry()).unwrap();
		let expected = Stub {
			reference: LinkReference::new(reference).unwrap(),
			neighbors: Some(Neighbors {
				next: next.map(CatalogId),
				previous: previous.map(CatalogId),
			}),
		};
		assert_eq!(Stub::decode(&record).unwrap(), Some(expected));
		let value = expected
			.catalog_value(CatalogId(cnid), ROOT_CREATED)
			.unwrap();
		let name: Vec<_> = record.key.name.units().collect();
		let mut bytes = vec![0xa5; native.len() + 1];
		let used = Record::encode(
			record.key.parent_or_thread,
			&name,
			value,
			geometry(),
			&mut bytes,
		)
		.unwrap();
		assert_eq!(&bytes[..used], native);
		assert_eq!(bytes[used], 0xa5);
	}

	for (span, cnid, count, head, data_bytes, resource_bytes, mode) in [
		(FIRST_INODE, 19, 3, 22, 22, 24, 0o100751),
		(SECOND_INODE, 23, 2, 25, 23, 0, 0o100640),
	] {
		let native = &RECORDS[span];
		let inode = Inode::decode(native, PRIVATE_DIRECTORY_ID, geometry())
			.unwrap()
			.unwrap();
		assert_eq!(inode.reference.get(), cnid);
		assert_eq!(inode.metadata.id, CatalogId(cnid));
		assert_eq!(inode.link_count.get(), count);
		assert_eq!(inode.first_link, Some(CatalogId(head)));
		assert_eq!(inode.data.logical_bytes, data_bytes);
		assert_eq!(inode.resource.logical_bytes, resource_bytes);
		assert_eq!(inode.metadata.permissions.mode, mode);
		let mut bytes = vec![0xa5; native.len() + 1];
		let used = inode
			.encode(PRIVATE_DIRECTORY_ID, geometry(), &mut bytes)
			.unwrap();
		assert_eq!(&bytes[..used], native);
		assert_eq!(bytes[used], 0xa5);
	}

	let attribute = InlineRecord::decode(&RECORDS[INODE_ATTRIBUTE]).unwrap();
	assert_eq!(attribute.key.file_id, CatalogId(19));
	assert!(
		attribute
			.key
			.name
			.units()
			.eq("org.pomologie.hardlink.sample".encode_utf16())
	);
	assert_eq!(attribute.value, &[0x00, 0x11, 0x22, 0xff]);
}

#[test]
fn accepts_a_chain_free_reference_distinct_from_the_payload_catalog_id() {
	let visible_bytes = &RECORDS[LEGACY_ONE];
	let visible = Record::decode(visible_bytes, geometry()).unwrap();
	let stub = Stub::decode(&visible).unwrap().unwrap();
	assert_eq!(stub.reference.get(), 100);
	assert_eq!(stub.neighbors, None);
	let value = stub.catalog_value(CatalogId(20), ROOT_CREATED).unwrap();
	let mut encoded = vec![0; visible_bytes.len()];
	let name: Vec<_> = visible.key.name.units().collect();
	let used = Record::encode(
		visible.key.parent_or_thread,
		&name,
		value,
		geometry(),
		&mut encoded,
	)
	.unwrap();
	assert_eq!(&encoded[..used], visible_bytes);

	let inode_bytes = &RECORDS[LEGACY_INODE];
	let inode = Inode::decode(inode_bytes, PRIVATE_DIRECTORY_ID, geometry())
		.unwrap()
		.unwrap();
	assert_eq!(inode.reference, stub.reference);
	assert_eq!(inode.first_link, None);
	assert_eq!(inode.link_count.get(), 3);
	assert_eq!(inode.metadata.id, CatalogId(19));
	let name = inode.reference.inode_name();
	let mut bytes = vec![0; inode_bytes.len()];
	let used = Record::encode(
		PRIVATE_DIRECTORY_ID,
		name.units(),
		inode.chain_free_value().unwrap(),
		geometry(),
		&mut bytes,
	)
	.unwrap();
	assert_eq!(&bytes[..used], inode_bytes);
}

#[test]
fn rejects_invalid_local_link_identity_and_formats_the_full_reference_range() {
	let mut inode = Inode::decode(&RECORDS[FIRST_INODE], PRIVATE_DIRECTORY_ID, geometry())
		.unwrap()
		.unwrap();
	let ordinary = Record::decode(&RECORDS[FIRST_INODE], geometry()).unwrap();
	assert_eq!(Stub::decode(&ordinary), Ok(None));

	let mut stub_record = Record::decode(&RECORDS[ONE], geometry()).unwrap();
	let Value::File {
		metadata, resource, ..
	} = stub_record.value
	else {
		panic!("missing native stub");
	};
	stub_record.value = Value::File {
		metadata,
		data: inode.data,
		resource,
	};
	assert_eq!(Stub::decode(&stub_record), Err(Error::InvalidStub));

	assert_eq!(inode.chain_free_value(), Err(Error::InvalidChain));
	inode.first_link = Some(inode.metadata.id);
	let mut destination = [0xa5; 512];
	assert_eq!(
		inode.encode(PRIVATE_DIRECTORY_ID, geometry(), &mut destination),
		Err(Error::InvalidChain)
	);
	assert_eq!(destination, [0xa5; 512]);

	let repeated = Some(CatalogId(21));
	let stub = Stub {
		reference: LinkReference::new(19).unwrap(),
		neighbors: Some(Neighbors {
			next: repeated,
			previous: repeated,
		}),
	};
	assert_eq!(
		stub.catalog_value(CatalogId(20), ROOT_CREATED),
		Err(Error::InvalidChain)
	);
	assert!(LinkReference::new(0).is_none());

	for raw in [1, 100, 1_073_741_923, u32::MAX] {
		let reference = LinkReference::new(raw).unwrap();
		let name = reference.inode_name();
		let len = Key::encode(PRIVATE_DIRECTORY_ID, name.units(), &mut destination).unwrap();
		let (key, _) = Key::split_leaf(&destination[..len]).unwrap();
		assert_eq!(LinkReference::from_inode_name(key.name), Some(reference));
		assert!(key.name.units().eq(format!("iNode{raw}").encode_utf16()));
	}

	for spelling in [
		"inode100",
		"iNode0",
		"iNode01",
		"iNode",
		"iNode-1",
		"iNode4294967296",
	] {
		let name: Vec<_> = spelling.encode_utf16().collect();
		let len = Key::encode(PRIVATE_DIRECTORY_ID, &name, &mut destination).unwrap();
		let (key, _) = Key::split_leaf(&destination[..len]).unwrap();
		assert!(LinkReference::from_inode_name(key.name).is_none());
	}

	inode.first_link = None;
	inode.link_count = NonZeroU32::MIN;
	assert!(inode.chain_free_value().is_ok());
}

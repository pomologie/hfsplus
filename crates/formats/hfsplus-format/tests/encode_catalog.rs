//! Catalog encoding coverage for H-FMT-02 and the record packing part of H-BUILD-03.

use hfsplus_format::{
	Error,
	btree::Node,
	catalog::{
		EncodeValue, FLAG_THREAD_EXISTS, Key, MAX_KEY_LENGTH, MAX_NAME_UNITS, Metadata,
		Permissions, Record, ThreadKind, Value,
	},
	volume::{AllocationBlock, CatalogId, Extent, Fork, Header},
};

const NATIVE_PRIMARY: &[u8] = include_bytes!("../../../../tests/fixtures/native-primary.bin");
const NATIVE_LEAF: &[u8] = include_bytes!("../../../../tests/fixtures/native-catalog-leaf.bin");
const NATIVE_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
const OPTICAL_PRIMARY: &[u8] = include_bytes!("../../../../tests/fixtures/optical-primary.bin");
const OPTICAL_LEAF: &[u8] = include_bytes!("../../../../tests/fixtures/optical-catalog-leaf.bin");
const OPTICAL_IMAGE_BYTES: u64 = 20_480;
const UNWRITTEN: u8 = 0xaa;
const TEST_NAME: &[u16] = &[b'A' as u16];
const ROOT_ID: CatalogId = CatalogId(2);
const FILE_ID: CatalogId = CatalogId(16);

fn metadata() -> Metadata {
	Metadata {
		id: FILE_ID,
		flags: FLAG_THREAD_EXISTS,
		dates: [1, 2, 3, 4, 5],
		permissions: Permissions {
			uid: 501,
			gid: 20,
			admin_flags: 0,
			owner_flags: 0,
			mode: 0o100_644,
			special: 0,
		},
		finder_info: [0; 32],
		encoding: 0,
	}
}

#[test]
fn encodes_all_native_catalog_record_kinds_byte_for_byte() {
	for (primary, leaf, image_bytes) in [
		(NATIVE_PRIMARY, NATIVE_LEAF, NATIVE_IMAGE_BYTES),
		(OPTICAL_PRIMARY, OPTICAL_LEAF, OPTICAL_IMAGE_BYTES),
	] {
		let geometry = Header::decode(primary, image_bytes).unwrap().geometry;
		let node = Node::decode(leaf).unwrap();
		for expected in node.records() {
			let record = Record::decode(expected, geometry).unwrap();
			let key_name: Vec<_> = record.key.name.units().collect();
			let thread_name: Vec<_>;
			let value = match record.value {
				Value::Folder {
					metadata,
					valence,
					tail_word,
				} => EncodeValue::Folder {
					metadata,
					valence,
					tail_word,
				},
				Value::File {
					metadata,
					data,
					resource,
				} => EncodeValue::File {
					metadata,
					data,
					resource,
				},
				Value::Thread { kind, parent, name } => {
					thread_name = name.units().collect();
					EncodeValue::Thread {
						kind,
						parent,
						name: &thread_name,
					}
				}
			};
			assert_eq!(Record::encoded_len(&key_name, value), Ok(expected.len()));
			let mut encoded = vec![UNWRITTEN; expected.len() + 1];
			let written = Record::encode(
				record.key.parent_or_thread,
				&key_name,
				value,
				geometry,
				&mut encoded,
			)
			.unwrap();
			assert_eq!(&encoded[..written], expected);
			assert_eq!(encoded[written], UNWRITTEN);
		}
	}
}

#[test]
fn encodes_catalog_key_with_big_endian_units_without_unicode_normalization() {
	const UNPAIRED_SURROGATE: u16 = 0xd800;
	const MAX_ENCODED_KEY_BYTES: usize = MAX_KEY_LENGTH + size_of::<u16>();
	let name = [UNPAIRED_SURROGATE; MAX_NAME_UNITS];
	let mut bytes = [UNWRITTEN; MAX_ENCODED_KEY_BYTES];
	assert_eq!(Key::encode(ROOT_ID, &name, &mut bytes), Ok(bytes.len()));
	let (key, remainder) = Key::split_leaf(&bytes).unwrap();
	assert_eq!(key.parent_or_thread, ROOT_ID);
	assert!(key.name.units().eq(name));
	assert!(remainder.is_empty());
	assert_eq!(&bytes[..2], &[0x02, 0x04]);
	assert_eq!(&bytes[bytes.len() - 2..], &UNPAIRED_SURROGATE.to_be_bytes());
	let previous = bytes;
	let oversized = [UNPAIRED_SURROGATE; MAX_NAME_UNITS + 1];
	assert_eq!(
		Key::encode(ROOT_ID, &oversized, &mut bytes),
		Err(Error::InvalidCatalog)
	);
	assert_eq!(Key::encode(ROOT_ID, &name, &mut []), Err(Error::Truncated));
	assert_eq!(bytes, previous);
}

#[test]
fn encodes_distinct_nonempty_forks_and_checks_geometry_before_writing() {
	let geometry = Header::decode(NATIVE_PRIMARY, NATIVE_IMAGE_BYTES)
		.unwrap()
		.geometry;
	let data = Fork::new(
		100,
		0,
		1,
		&[Extent {
			start: AllocationBlock(3),
			blocks: 1,
		}],
		geometry,
	)
	.unwrap();
	let mut resource = Fork::new(
		200,
		0,
		1,
		&[Extent {
			start: AllocationBlock(5),
			blocks: 1,
		}],
		geometry,
	)
	.unwrap();
	let file = EncodeValue::File {
		metadata: metadata(),
		data,
		resource,
	};
	let len = Record::encoded_len(TEST_NAME, file).unwrap();
	let mut encoded = vec![UNWRITTEN; len];
	assert_eq!(
		Record::encode(ROOT_ID, TEST_NAME, file, geometry, &mut encoded),
		Ok(len)
	);
	let record = Record::decode(&encoded, geometry).unwrap();
	assert_eq!(
		record.value,
		Value::File {
			metadata: metadata(),
			data,
			resource
		}
	);

	resource.logical_bytes = u64::MAX;
	let invalid_file = EncodeValue::File {
		metadata: metadata(),
		data,
		resource,
	};
	encoded.fill(UNWRITTEN);
	assert_eq!(
		Record::encode(ROOT_ID, TEST_NAME, invalid_file, geometry, &mut encoded),
		Err(Error::InvalidFork)
	);
	assert!(encoded.iter().all(|byte| *byte == UNWRITTEN));
}

#[test]
fn rejects_bad_key_value_shapes_and_short_destinations_without_writes() {
	let geometry = Header::decode(NATIVE_PRIMARY, NATIVE_IMAGE_BYTES)
		.unwrap()
		.geometry;
	let mut file_metadata = metadata();
	file_metadata.flags &= !FLAG_THREAD_EXISTS;
	let invalid_file = EncodeValue::File {
		metadata: file_metadata,
		data: Fork::empty(),
		resource: Fork::empty(),
	};
	let folder = EncodeValue::Folder {
		metadata: metadata(),
		valence: 0,
		tail_word: 0,
	};
	let thread = EncodeValue::Thread {
		kind: ThreadKind::File,
		parent: ROOT_ID,
		name: TEST_NAME,
	};
	let invalid_thread = EncodeValue::Thread {
		kind: ThreadKind::Folder,
		parent: ROOT_ID,
		name: &[],
	};
	let mut encoded = [UNWRITTEN; 512];
	for (key_name, value) in [
		(&[][..], folder),
		(&[][..], invalid_thread),
		(TEST_NAME, thread),
		(TEST_NAME, invalid_file),
	] {
		assert_eq!(
			Record::encode(ROOT_ID, key_name, value, geometry, &mut encoded),
			Err(Error::InvalidCatalog)
		);
	}
	let short_len = Record::encoded_len(&[], thread).unwrap() - 1;
	assert_eq!(
		Record::encode(FILE_ID, &[], thread, geometry, &mut encoded[..short_len]),
		Err(Error::Truncated)
	);
	assert!(encoded.iter().all(|byte| *byte == UNWRITTEN));
}

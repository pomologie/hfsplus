use hfsplus_format::{
	Error,
	volume::{AllocationBlock, CatalogId, Extent, Fork, Geometry, Header, Signature, SpecialFiles},
};

const NATIVE_PRIMARY: &[u8] = include_bytes!("fixtures/native-primary.bin");
const NATIVE_ALTERNATE: &[u8] = include_bytes!("fixtures/native-alternate.bin");
const OPTICAL_PRIMARY: &[u8] = include_bytes!("fixtures/optical-primary.bin");
const NATIVE_VOLUME_BYTES: u64 = 8 * 1024 * 1024;
const OPTICAL_VOLUME_BYTES: u64 = 20_480;
const NATIVE_BLOCK_BYTES: u32 = 4096;
const NATIVE_BLOCK_COUNT: u32 = 2048;
const RESERVED_JOURNAL_FIELD: std::ops::Range<usize> = 12..16;

#[test]
fn encodes_native_headers_byte_for_byte() {
	for (expected, volume_bytes) in [
		(NATIVE_PRIMARY, NATIVE_VOLUME_BYTES),
		(NATIVE_ALTERNATE, NATIVE_VOLUME_BYTES),
		(OPTICAL_PRIMARY, OPTICAL_VOLUME_BYTES),
	] {
		let header = Header::decode(expected, volume_bytes).unwrap();
		assert_eq!(header.encode().unwrap(), expected);
	}
}

#[test]
fn preserves_the_unjournaled_reserved_word() {
	let mut expected = NATIVE_PRIMARY.to_vec();
	expected[RESERVED_JOURNAL_FIELD].copy_from_slice(b"test");
	let header = Header::decode(&expected, NATIVE_VOLUME_BYTES).unwrap();
	assert!(header.journal_info.is_none());
	assert_eq!(header.encode().unwrap().as_slice(), expected);
}

#[test]
fn constructs_and_rechecks_the_header_and_fork() {
	let geometry =
		Geometry::new(NATIVE_BLOCK_BYTES, NATIVE_BLOCK_COUNT, NATIVE_VOLUME_BYTES).unwrap();
	let extent = Extent {
		start: AllocationBlock(4),
		blocks: 1,
	};
	let fork = Fork::new(23, geometry.block_size(), 1, &[extent], geometry).unwrap();
	assert_eq!(
		Fork::decode(&fork.encode(geometry).unwrap(), geometry).unwrap(),
		fork,
	);
	let overlap = [extent; 2];
	assert_eq!(
		Fork::new(23, geometry.block_size(), 2, &overlap, geometry),
		Err(Error::InvalidFork),
	);

	let mut header = Header::new_clean(
		geometry,
		SpecialFiles {
			allocation: Fork::empty(),
			extents: Fork::empty(),
			catalog: fork,
			attributes: Fork::empty(),
			startup: Fork::empty(),
		},
	)
	.unwrap();
	assert_eq!(header.signature, Signature::HfsPlus);
	assert_eq!(header.next_catalog_id, CatalogId::FIRST_USER);
	assert!(header.unmounted_cleanly());
	assert_eq!(
		Header::decode(&header.encode().unwrap(), NATIVE_VOLUME_BYTES).unwrap(),
		header,
	);
	header.journal_info = Some(AllocationBlock(1));
	assert_eq!(header.encode(), Err(Error::InvalidGeometry));
	header.journal_info = None;
	header.forks.catalog.logical_bytes = u64::from(geometry.block_size()) + 1;
	assert_eq!(header.encode(), Err(Error::InvalidFork));
}

use hfsplus_format::{
	Error,
	volume::{
		AllocationBlock, CatalogId, Extent, FORK_BYTES, Fork, Geometry, HEADER_BYTES, Header,
		Signature, SpecialFiles,
	},
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

#[test]
fn embedded_fork_slots_preserve_logical_order_and_report_overflow() {
	const PHYSICAL_STARTS: [u32; 8] = [90, 12, 75, 33, 45, 66, 20, 10];
	const CLUMP_BLOCKS: u32 = 4;
	let geometry =
		Geometry::new(NATIVE_BLOCK_BYTES, NATIVE_BLOCK_COUNT, NATIVE_VOLUME_BYTES).unwrap();
	let extents = PHYSICAL_STARTS.map(|block| Extent {
		start: AllocationBlock(block),
		blocks: 1,
	});
	for extent_count in 0..=extents.len() {
		let entries = &extents[..extent_count];
		let embedded = extent_count as u32;
		let maximum_total = embedded + u32::from(extent_count == extents.len());
		for total in embedded..=maximum_total {
			let logical_bytes = u64::from(total) * u64::from(NATIVE_BLOCK_BYTES);
			let clump = CLUMP_BLOCKS * NATIVE_BLOCK_BYTES;
			let fork = Fork::new(logical_bytes, clump, total, entries, geometry).unwrap();
			let mut expected = Vec::with_capacity(FORK_BYTES);
			expected.extend(logical_bytes.to_be_bytes());
			expected.extend(clump.to_be_bytes());
			expected.extend(total.to_be_bytes());
			for extent in entries {
				expected.extend(extent.start.0.to_be_bytes());
				expected.extend(extent.blocks.to_be_bytes());
			}
			expected.resize(FORK_BYTES, 0);
			assert_eq!(fork.encode(geometry).unwrap().as_slice(), expected);
			let decoded = Fork::decode(&expected, geometry).unwrap();
			assert!(decoded.extents().eq(entries.iter().copied()));
			assert_eq!(decoded.has_overflow(), total != embedded);
		}
	}
}

#[test]
fn rejects_every_short_prefix_of_a_native_header_and_fork() {
	let header = Header::decode(NATIVE_PRIMARY, NATIVE_VOLUME_BYTES).unwrap();
	let catalog = header.forks.catalog.encode(header.geometry).unwrap();
	for length in 0..HEADER_BYTES {
		assert_eq!(
			Header::decode(&NATIVE_PRIMARY[..length], NATIVE_VOLUME_BYTES),
			Err(Error::Truncated),
			"header prefix of {length} bytes",
		);
	}
	for length in 0..FORK_BYTES {
		assert_eq!(
			Fork::decode(&catalog[..length], header.geometry),
			Err(Error::Truncated),
			"fork prefix of {length} bytes",
		);
	}
	assert_eq!(
		Fork::decode(&catalog, header.geometry).unwrap(),
		header.forks.catalog,
	);
}

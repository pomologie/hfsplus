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
fn reports_native_volume_fields_for_the_wire_reference() {
	let captures = [
		("native-primary.bin", NATIVE_PRIMARY, NATIVE_VOLUME_BYTES),
		(
			"native-alternate.bin",
			NATIVE_ALTERNATE,
			NATIVE_VOLUME_BYTES,
		),
		("optical-primary.bin", OPTICAL_PRIMARY, OPTICAL_VOLUME_BYTES),
		(
			"native-512k-primary.bin",
			include_bytes!("fixtures/native-512k-primary.bin"),
			512 << 10,
		),
		(
			"native-8m-primary.bin",
			include_bytes!("fixtures/native-8m-primary.bin"),
			NATIVE_VOLUME_BYTES,
		),
		(
			"native-128m-primary.bin",
			include_bytes!("fixtures/native-128m-primary.bin"),
			128 << 20,
		),
	];
	for (name, wire, device_bytes) in captures {
		let header = Header::decode(wire, device_bytes).unwrap();
		let geometry = header.geometry;
		assert_eq!(header.encode().unwrap().as_slice(), wire);
		println!(
			"HFS+ header {name} {:?} {} {} {} {} {} {}",
			header.signature,
			geometry.block_size(),
			geometry.total_blocks(),
			header.free_blocks,
			header.next_allocation.0,
			geometry.allocation_bytes(),
			geometry.alternate_header_offset(),
		);
		for (role, fork) in [
			("allocation", header.forks.allocation),
			("extents", header.forks.extents),
			("catalog", header.forks.catalog),
			("attributes", header.forks.attributes),
			("startup", header.forks.startup),
		] {
			print!(
				"HFS+ fork {name} {role} {} {} {}",
				fork.logical_bytes, fork.clump_bytes, fork.total_blocks,
			);
			for extent in fork.extents() {
				print!(" {}:{}", extent.start.0, extent.blocks);
			}
			println!();
		}
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
fn fork_encoding_checks_capacity_of_the_receiving_geometry() {
	const SMALL_VOLUME_BLOCKS: u32 = 4;
	const FORK_BLOCKS: u32 = SMALL_VOLUME_BLOCKS + 1;
	let original =
		Geometry::new(NATIVE_BLOCK_BYTES, NATIVE_BLOCK_COUNT, NATIVE_VOLUME_BYTES).unwrap();
	let receiving = Geometry::new(
		NATIVE_BLOCK_BYTES,
		SMALL_VOLUME_BLOCKS,
		u64::from(NATIVE_BLOCK_BYTES) * u64::from(SMALL_VOLUME_BLOCKS),
	)
	.unwrap();
	let fork = Fork::new(
		u64::from(NATIVE_BLOCK_BYTES) * u64::from(FORK_BLOCKS),
		NATIVE_BLOCK_BYTES,
		FORK_BLOCKS,
		&[Extent {
			start: AllocationBlock(1),
			blocks: FORK_BLOCKS,
		}],
		original,
	)
	.unwrap();
	let encoded = fork.encode(original).unwrap();
	assert_eq!(Fork::decode(&encoded, original), Ok(fork));
	assert_eq!(fork.validate_geometry(receiving), Err(Error::InvalidFork));
	assert_eq!(fork.encode(receiving), Err(Error::InvalidFork));
}

#[test]
fn volume_wire_shapes_match_the_reference_model() {
	const ALLOCATION_BLOCK_SIZES: [u32; 3] = [512, 4096, 65_536];
	const PHYSICAL_STARTS: [u32; 8] = [90, 12, 75, 33, 45, 66, 20, 112];
	const EXTENT_ROTATIONS: [usize; 3] = [0, 3, 7];
	const EXPECTED_FORK_CASES: usize = 171;
	let mut cases = 0;
	for block_bytes in ALLOCATION_BLOCK_SIZES {
		let allocation_bytes = u128::from(NATIVE_BLOCK_COUNT) * u128::from(block_bytes);
		let device_bytes = u64::try_from(allocation_bytes + u128::from(block_bytes - 1)).unwrap();
		let geometry = Geometry::new(block_bytes, NATIVE_BLOCK_COUNT, device_bytes).unwrap();
		assert_eq!(u128::from(geometry.allocation_bytes()), allocation_bytes);
		for rotation in EXTENT_ROTATIONS {
			let extents: [Extent; PHYSICAL_STARTS.len()] = std::array::from_fn(|index| Extent {
				start: AllocationBlock(PHYSICAL_STARTS[(index + rotation) % PHYSICAL_STARTS.len()]),
				blocks: 1 + u32::try_from(index % 3).unwrap(),
			});
			for extent in &extents {
				let range = geometry.block_range(extent.start, extent.blocks).unwrap();
				let start = u128::from(extent.start.0) * u128::from(block_bytes);
				let end = start + u128::from(extent.blocks) * u128::from(block_bytes);
				assert_eq!(u128::from(range.start)..u128::from(range.end), start..end);
			}
			cases += match_fork_wire_shapes(&extents, geometry);
		}
	}
	assert_eq!(cases, EXPECTED_FORK_CASES);
	let prefixes = check_native_header_and_fork_prefixes();
	assert_eq!(prefixes, HEADER_BYTES + FORK_BYTES);
	println!("HFS+ property cases checked: {}", cases + prefixes);
}

fn match_fork_wire_shapes(extents: &[Extent], geometry: Geometry) -> usize {
	const CLUMP_BLOCKS: u32 = 4;
	let mut cases = 0;
	for extent_count in 0..=extents.len() {
		let entries = &extents[..extent_count];
		let embedded = entries.iter().map(|extent| extent.blocks).sum::<u32>();
		let maximum_total = embedded + u32::from(extent_count == extents.len());
		for total in embedded..=maximum_total {
			let allocation_bytes = u64::from(total) * u64::from(geometry.block_size());
			let partial = allocation_bytes.saturating_sub(u64::from(geometry.block_size() - 1));
			let logical_sizes = [allocation_bytes, partial];
			for logical_bytes in logical_sizes.into_iter().take(1 + usize::from(total != 0)) {
				let clump = CLUMP_BLOCKS * geometry.block_size();
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
				assert_eq!(decoded, fork);
				assert!(decoded.extents().eq(entries.iter().copied()));
				assert_eq!(decoded.has_overflow(), total != embedded);
				cases += 1;
			}
		}
	}
	cases
}

fn check_native_header_and_fork_prefixes() -> usize {
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
	HEADER_BYTES + FORK_BYTES
}

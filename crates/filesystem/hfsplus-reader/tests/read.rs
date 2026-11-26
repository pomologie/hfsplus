use std::{
	cell::{Cell, RefCell},
	collections::BTreeSet,
};

use hfsplus_core::{ByteLen, ByteOffset, RangeError, ReadAt};
use hfsplus_format::{
	Error as FormatError,
	btree::{Kind, Node, TreeHeader},
	catalog::{Record, Value},
	volume::{
		AllocationBlock, CLEAN_UNMOUNT_ATTRIBUTE, Extent, Fork, HEADER_BYTES, Header,
		INCONSISTENT_ATTRIBUTE, JOURNALED_ATTRIBUTE, PRIMARY_HEADER_OFFSET,
		SPARED_BLOCKS_ATTRIBUTE, Signature, UNUSED_NODE_FIX_ATTRIBUTE,
	},
};
use hfsplus_reader::{AlternateHeader, Error, ForkReadSupport, SpecialFile, Volume};

const OPTICAL: &[u8] = include_bytes!("fixtures/optical-volume.bin");
const USER_CONTENT: &[u8] = b"pomologie fixture\n";
const FILE_NAME: &str = "hello.txt";
const BTREE_PROBE_BYTES: usize = 34;
const PRIMARY: usize = PRIMARY_HEADER_OFFSET as usize;
const VOLUME_ATTRIBUTE_OFFSET: usize = PRIMARY + 4;
const JOURNAL_BLOCK_OFFSET: usize = PRIMARY + 12;
const ALTERNATE_SIGNATURE_BYTES: usize = 2;
const UNQUALIFIED_ATTRIBUTE: u32 = 1 << 2;
const FIRST_TEST_DATA_BLOCK: AllocationBlock = AllocationBlock(5);
const SECOND_TEST_DATA_BLOCK: AllocationBlock = AllocationBlock(1);

#[test]
fn observes_boot_and_trailing_reserved_sectors_independently() {
	let volume = Volume::open(OPTICAL).unwrap();
	assert!(volume.reserved_sectors_are_clear().unwrap());
	let trailing = volume.header().geometry.alternate_header_offset() as usize + HEADER_BYTES;
	for offset in [PRIMARY - 1, trailing] {
		let mut image = OPTICAL.to_vec();
		image[offset] = 1;
		let changed = Volume::open(image.as_slice()).unwrap();
		assert!(!changed.reserved_sectors_are_clear().unwrap());
	}
}

#[test]
fn reads_native_catalog_and_file_bytes() {
	let volume = Volume::open(OPTICAL).unwrap();
	assert_eq!(volume.header().signature, Signature::HfsPlus);
	assert_eq!(volume.alternate_header(), AlternateHeader::MatchesPrimary);
	assert_eq!(volume.fork_read_support(), ForkReadSupport::Supported);
	let catalog = volume.open_special_fork(SpecialFile::Catalog).unwrap();
	let mut prefix = [0; BTREE_PROBE_BYTES];
	catalog.read_exact_at(ByteOffset(0), &mut prefix).unwrap();
	let node_len = usize::from(TreeHeader::node_size_hint(&prefix).unwrap());
	let mut node = vec![0; node_len];
	catalog.read_exact_at(ByteOffset(0), &mut node).unwrap();
	let header_node = Node::decode(&node).unwrap();
	let tree = TreeHeader::decode(&header_node, catalog.len().0).unwrap();
	let leaf_offset = u64::from(tree.first_leaf.unwrap().0) * node_len as u64;
	catalog
		.read_exact_at(ByteOffset(leaf_offset), &mut node)
		.unwrap();
	let leaf = Node::decode(&node).unwrap();
	assert_eq!(leaf.kind(), Kind::Leaf);
	let matching_file = leaf
		.records()
		.map(|raw| Record::decode(raw, volume.header().geometry).unwrap())
		.find(|record| record.key.name.units().eq(FILE_NAME.encode_utf16()))
		.unwrap();
	let Value::File { data, resource, .. } = matching_file.value else {
		panic!("native fixture entry is not a file");
	};
	let file = volume.open_embedded_fork(&data).unwrap();
	let mut bytes = vec![0; USER_CONTENT.len()];
	file.read_exact_at(ByteOffset(0), &mut bytes).unwrap();
	assert_eq!(bytes, USER_CONTENT);
	assert!(
		file.read_exact_at(ByteOffset(file.len().0), &mut [])
			.is_ok()
	);
	assert!(matches!(
		file.read_exact_at(ByteOffset(file.len().0), &mut [0]),
		Err(Error::Bounds(RangeError::OutsideSource))
	));
	assert!(volume.open_embedded_fork(&resource).unwrap().is_empty());
}

#[test]
fn reads_discontiguous_extents_in_logical_order_and_excludes_slack() {
	let volume = Volume::open(OPTICAL).unwrap();
	let geometry = volume.header().geometry;
	let block_bytes = u64::from(geometry.block_size());
	let first = geometry.block_range(FIRST_TEST_DATA_BLOCK, 1).unwrap();
	let second = geometry.block_range(SECOND_TEST_DATA_BLOCK, 1).unwrap();
	let logical_tail = 13usize;
	let extents = [
		Extent {
			start: FIRST_TEST_DATA_BLOCK,
			blocks: 1,
		},
		Extent {
			start: SECOND_TEST_DATA_BLOCK,
			blocks: 1,
		},
	];
	let descriptor =
		Fork::new(block_bytes + logical_tail as u64, 0, 2, &extents, geometry).unwrap();
	let file = volume.open_embedded_fork(&descriptor).unwrap();
	let mut spanning_read = [0; 18];
	let first_chunk = spanning_read.len() - logical_tail;
	file.read_exact_at(
		ByteOffset(block_bytes - first_chunk as u64),
		&mut spanning_read,
	)
	.unwrap();
	assert_eq!(
		&spanning_read[..first_chunk],
		&OPTICAL[first.end as usize - first_chunk..first.end as usize]
	);
	assert_eq!(
		&spanning_read[first_chunk..],
		&OPTICAL[second.start as usize..second.start as usize + logical_tail]
	);
	assert!(matches!(
		file.read_exact_at(ByteOffset(u64::MAX), &mut [0]),
		Err(Error::Bounds(RangeError::Overflow))
	));
	assert!(matches!(
		file.read_exact_at(ByteOffset(file.len().0 - 1), &mut [0; 2]),
		Err(Error::Bounds(RangeError::OutsideSource))
	));
}

#[test]
fn identifies_alternate_copies_without_using_them_for_fork_reads() {
	let mut image = OPTICAL.to_vec();
	let primary = Header::decode(&image[PRIMARY..], image.len() as u64).unwrap();
	let alternate_start = primary.geometry.alternate_header_offset() as usize;
	let alternate_range = alternate_start..alternate_start + HEADER_BYTES;
	let mut old = primary;
	old.writes = primary.writes + 1;
	image[alternate_range.clone()].copy_from_slice(&old.encode().unwrap());
	let source = InstrumentedSource::new(&image);
	let volume = Volume::open(&source).unwrap();
	assert_eq!(volume.header(), &primary);
	assert_eq!(volume.alternate_header(), AlternateHeader::Differs(&old));
	assert_eq!(
		&*source.reads.borrow(),
		&[
			(ByteOffset(PRIMARY_HEADER_OFFSET), HEADER_BYTES),
			(ByteOffset(alternate_start as u64), HEADER_BYTES),
		],
	);
	assert!(volume.open_special_fork(SpecialFile::Catalog).is_ok());

	image[alternate_range.clone()].fill(0);
	let volume = Volume::open(image.as_slice()).unwrap();
	assert_eq!(volume.alternate_header(), AlternateHeader::Blank);
	assert!(volume.open_special_fork(SpecialFile::Catalog).is_ok());

	image[alternate_start..alternate_start + ALTERNATE_SIGNATURE_BYTES].fill(0xff);
	let volume = Volume::open(image.as_slice()).unwrap();
	assert_eq!(
		volume.alternate_header(),
		AlternateHeader::Invalid(FormatError::InvalidSignature)
	);
	assert_eq!(volume.header(), &primary);
}

#[test]
fn permits_tail_bytes_shorter_than_an_allocation_block() {
	const SUB_ALLOCATION_TAIL: usize = 512;
	let mut image = OPTICAL.to_vec();
	let original = Volume::open(OPTICAL).unwrap();
	let previous = original.header().geometry.alternate_header_offset() as usize;
	let next = previous + SUB_ALLOCATION_TAIL;
	image.resize(image.len() + SUB_ALLOCATION_TAIL, 0);
	image[previous..next].fill(0);
	image[next..next + HEADER_BYTES].copy_from_slice(&OPTICAL[previous..previous + HEADER_BYTES]);
	let with_tail = Volume::open(image.as_slice()).unwrap();
	assert_eq!(
		with_tail.alternate_header(),
		AlternateHeader::MatchesPrimary
	);
	assert_eq!(
		with_tail.header().geometry.alternate_header_offset(),
		next as u64
	);
	assert!(with_tail.open_special_fork(SpecialFile::Catalog).is_ok());
}

#[test]
fn rejects_input_containing_whole_blocks_beyond_the_bounded_volume() {
	let original = Volume::open(OPTICAL).unwrap();
	let mut image = OPTICAL.to_vec();
	image.resize(
		image.len() + original.header().geometry.block_size() as usize,
		0,
	);
	assert!(matches!(
		Volume::open(image.as_slice()),
		Err(Error::Format(FormatError::InvalidGeometry))
	));
}

#[test]
fn reports_unsupported_volume_state_before_exposing_forks() {
	const NATIVE_ATTRS: usize = 4;
	let native_header = include_bytes!("fixtures/native-primary.bin");
	let mut image = OPTICAL.to_vec();
	let journal_block = 1u32;
	image[JOURNAL_BLOCK_OFFSET..][..size_of::<u32>()].copy_from_slice(&journal_block.to_be_bytes());
	let expectations = [
		(0, ForkReadSupport::Dirty),
		(
			CLEAN_UNMOUNT_ATTRIBUTE | INCONSISTENT_ATTRIBUTE,
			ForkReadSupport::Dirty,
		),
		(JOURNALED_ATTRIBUTE, ForkReadSupport::NeedsJournalReplay),
		(
			CLEAN_UNMOUNT_ATTRIBUTE | JOURNALED_ATTRIBUTE,
			ForkReadSupport::UnverifiedJournal,
		),
		(
			CLEAN_UNMOUNT_ATTRIBUTE | UNQUALIFIED_ATTRIBUTE,
			ForkReadSupport::UnqualifiedAttributes(UNQUALIFIED_ATTRIBUTE),
		),
		(
			CLEAN_UNMOUNT_ATTRIBUTE | SPARED_BLOCKS_ATTRIBUTE,
			ForkReadSupport::SparedBlocks,
		),
	];
	for (flags, expected) in expectations {
		image[VOLUME_ATTRIBUTE_OFFSET..][..size_of::<u32>()].copy_from_slice(&flags.to_be_bytes());
		let volume = Volume::open(image.as_slice()).unwrap();
		assert_eq!(volume.fork_read_support(), expected);
		assert!(matches!(
			volume.open_special_fork(SpecialFile::Catalog),
			Err(Error::UnsupportedVolume(reason)) if reason == expected
		));
	}
	let native_flags = &native_header[NATIVE_ATTRS..][..size_of::<u32>()];
	image[VOLUME_ATTRIBUTE_OFFSET..][..size_of::<u32>()].copy_from_slice(native_flags);
	let native_profile = Volume::open(image.as_slice()).unwrap();
	assert_eq!(
		native_profile.header().attributes,
		CLEAN_UNMOUNT_ATTRIBUTE | UNUSED_NODE_FIX_ATTRIBUTE
	);
	assert_eq!(
		native_profile.fork_read_support(),
		ForkReadSupport::Supported
	);
}

#[test]
fn refuses_overflow_extents_or_forks_in_reserved_blocks() {
	const EMBEDDED_EXTENTS: usize = 8;
	let volume = Volume::open(OPTICAL).unwrap();
	let geometry = volume.header().geometry;
	let block_bytes = u64::from(geometry.block_size());
	let extents: Vec<_> = (1..=EMBEDDED_EXTENTS as u32)
		.map(|block| Extent {
			start: AllocationBlock(block),
			blocks: 1,
		})
		.collect();
	let allocated = EMBEDDED_EXTENTS as u32 + 1;
	let overflow = Fork::new(
		u64::from(allocated) * block_bytes,
		0,
		allocated,
		&extents,
		geometry,
	)
	.unwrap();
	assert!(matches!(
		volume.open_embedded_fork(&overflow),
		Err(Error::OverflowExtents)
	));
	for reserved in [
		AllocationBlock(0),
		AllocationBlock(geometry.total_blocks() - 1),
	] {
		let fork = Fork::new(
			1,
			0,
			1,
			&[Extent {
				start: reserved,
				blocks: 1,
			}],
			geometry,
		)
		.unwrap();
		assert!(matches!(
			volume.open_embedded_fork(&fork),
			Err(Error::Format(FormatError::InvalidFork))
		));
	}
	let mut modified = volume.header().forks.catalog;
	modified.logical_bytes += 1;
	assert!(matches!(
		volume.open_embedded_fork(&modified),
		Err(Error::Format(FormatError::InvalidFork))
	));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
	UnchangedBounds(RangeError),
	SourceChanged,
	ShortRead,
}

struct InstrumentedSource<'a> {
	bytes: &'a [u8],
	failure: Cell<Option<(ByteOffset, Fault)>>,
	reads: RefCell<Vec<(ByteOffset, usize)>>,
}

impl<'a> InstrumentedSource<'a> {
	fn new(bytes: &'a [u8]) -> Self {
		Self {
			bytes,
			failure: Cell::new(None),
			reads: RefCell::default(),
		}
	}
}

impl ReadAt for InstrumentedSource<'_> {
	type Error = Fault;

	fn len(&self) -> ByteLen {
		self.bytes.len().into()
	}

	fn read_exact_at(&self, offset: ByteOffset, destination: &mut [u8]) -> Result<(), Self::Error> {
		self.reads.borrow_mut().push((offset, destination.len()));
		if let Some((failed_offset, error)) = self.failure.get()
			&& failed_offset == offset
		{
			return Err(error);
		}
		self.bytes
			.read_exact_at(offset, destination)
			.map_err(Fault::UnchangedBounds)
	}
}

#[test]
fn verifies_catalog_fork_coverage_before_reading_native_volume() {
	// These values come from the native raw fixture, independent of Header::decode.
	const BLOCK_BYTES: u64 = 2048;
	const VOLUME_BLOCKS: u32 = 10;
	const CATALOG_BLOCKS: u32 = 4;
	const CATALOG_START: u32 = 1;
	const LOGICAL_BYTES: u64 = 8192;
	const CATALOG_LOGICAL_BYTES_OFFSET: usize = PRIMARY + 272;
	const CATALOG_FIRST_EXTENT_OFFSET: usize = CATALOG_LOGICAL_BYTES_OFFSET + 16;
	const NATIVE_ALTERNATE: u64 = 19_456;

	let available: BTreeSet<_> = (0..u64::from(VOLUME_BLOCKS)).collect();
	let cases = [
		(CATALOG_START, LOGICAL_BYTES),
		(VOLUME_BLOCKS - 1, LOGICAL_BYTES),
		(u32::MAX, LOGICAL_BYTES),
		(CATALOG_START, LOGICAL_BYTES + 1),
	];
	for (start, logical_bytes) in cases {
		let first = u64::from(start);
		let end = first + u64::from(CATALOG_BLOCKS);
		let claimed: BTreeSet<_> = (first..end).collect();
		let covered =
			claimed.is_subset(&available) && logical_bytes <= claimed.len() as u64 * BLOCK_BYTES;

		let mut image = OPTICAL.to_vec();
		image[CATALOG_LOGICAL_BYTES_OFFSET..][..size_of::<u64>()]
			.copy_from_slice(&logical_bytes.to_be_bytes());
		image[CATALOG_FIRST_EXTENT_OFFSET..][..size_of::<u32>()]
			.copy_from_slice(&start.to_be_bytes());
		let source = InstrumentedSource::new(&image);
		let primary_read = (ByteOffset(PRIMARY_HEADER_OFFSET), HEADER_BYTES);
		match Volume::open(&source) {
			Ok(volume) => {
				assert!(covered, "admitted uncovered catalog extent or logical size");
				let catalog = volume.open_special_fork(SpecialFile::Catalog).unwrap();
				let mut prefix = [0; BTREE_PROBE_BYTES];
				catalog.read_exact_at(ByteOffset(0), &mut prefix).unwrap();
				assert_eq!(TreeHeader::node_size_hint(&prefix), Ok(4096));
				assert_eq!(
					&*source.reads.borrow(),
					&[
						primary_read,
						(ByteOffset(NATIVE_ALTERNATE), HEADER_BYTES),
						(ByteOffset(first * BLOCK_BYTES), BTREE_PROBE_BYTES),
					],
				);
			}
			Err(error) => {
				assert!(!covered, "native fixture rejected: {error:?}");
				assert!(matches!(error, Error::Format(FormatError::InvalidFork)));
				assert_eq!(&*source.reads.borrow(), &[primary_read]);
			}
		}
	}
}

#[test]
fn discovery_is_bounded_and_source_failures_preserve_offset_and_cause() {
	let source = InstrumentedSource::new(OPTICAL);
	let volume = Volume::open(&source).unwrap();
	let geometry = volume.header().geometry;
	let alternate = ByteOffset(geometry.alternate_header_offset());
	assert_eq!(
		&*source.reads.borrow(),
		&[
			(ByteOffset(PRIMARY_HEADER_OFFSET), HEADER_BYTES),
			(alternate, HEADER_BYTES)
		]
	);
	let fork = volume.open_special_fork(SpecialFile::Catalog).unwrap();
	let first_extent = volume.header().forks.catalog.extents().next().unwrap();
	let physical = ByteOffset(
		geometry
			.block_range(first_extent.start, first_extent.blocks)
			.unwrap()
			.start,
	);
	for failure in [Fault::ShortRead, Fault::SourceChanged] {
		source.failure.set(Some((physical, failure)));
		assert!(matches!(
			fork.read_exact_at(ByteOffset(0), &mut [0]),
			Err(Error::Source { offset, error }) if offset == physical && error == failure
		));
	}
	source.failure.set(Some((alternate, Fault::ShortRead)));
	assert!(matches!(
		Volume::open(&source),
		Err(Error::Source { offset, error: Fault::ShortRead }) if offset == alternate
	));
}

#[test]
fn rejects_truncation_before_any_read_can_escape_the_source() {
	let header_end = PRIMARY + HEADER_BYTES;
	for length in [0, header_end - 1] {
		let tiny = InstrumentedSource::new(&OPTICAL[..length]);
		assert!(matches!(
			Volume::open(&tiny),
			Err(Error::Bounds(RangeError::OutsideSource))
		));
		assert!(tiny.reads.borrow().is_empty());
	}
	const NATIVE_SECTOR_BYTES: usize = 512;
	for missing in [1, NATIVE_SECTOR_BYTES] {
		let truncated = InstrumentedSource::new(&OPTICAL[..OPTICAL.len() - missing]);
		assert!(matches!(
			Volume::open(&truncated),
			Err(Error::Format(FormatError::InvalidGeometry))
		));
		assert_eq!(
			&*truncated.reads.borrow(),
			&[(ByteOffset(PRIMARY_HEADER_OFFSET), HEADER_BYTES)],
		);
	}
}

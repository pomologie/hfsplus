//! H-READ-01-T01, H-READ-02-T01 and H-READ-04-T01 native attribute evidence.

use hfsplus_core::{ByteLen, ByteOffset, RangeError, ReadAt};
use hfsplus_format::{
	Error as FormatError,
	attributes::{COMPRESSED_ATTRIBUTE_NAME, Key, NATIVE_NODE_BYTES},
	btree::{Kind, LEAF_HEIGHT, Node, NodeId, NodeLinks},
	volume::{AllocationBlock, CatalogId, Extent},
};
use hfsplus_reader::{
	Error as VolumeError, Volume,
	allocation::AllocationError,
	attributes::{
		AttributeEncoding, AttributeReadError, AttributeReadLimits, MAX_OWNED_ATTRIBUTE_BYTES,
	},
	check::{AllocationOwner, AttributeCoverage, CheckError, CheckLimit, CheckLimits},
	overflow::{ForkKind, OverflowError},
};

#[path = "attribute_forks/slices.rs"]
mod slices;

use slices::{Slices, patch};

const IMAGE: &[u8] = include_bytes!("fixtures/native-attribute-forks.bin");
const OWNER: CatalogId = CatalogId(18);
const BALLAST_OWNER: CatalogId = CatalogId(20);
const SAMPLE_DATA_BLOCK: AllocationBlock = AllocationBlock(738);
const CONTIGUOUS_ORDINAL: usize = 1;
const NATIVE_INLINE_NAMES: usize = 99;
const NATIVE_FORK_NAMES: usize = 2;
const NATIVE_METADATA_BLOCKS: u32 = 99;
const NATIVE_FILE_BLOCKS: u32 = 3895;
const NATIVE_FREE_BLOCKS: u32 = 70;
const INLINE: &str = "org.pomologie.inline";
const CONTIGUOUS: &str = "org.pomologie.contiguous";
const FRAGMENTED: &str = "org.pomologie.fragmented";
const BLOCK_BYTES: usize = 4096;
const CONTIGUOUS_BYTES: usize = BLOCK_BYTES + 1;
const CONTIGUOUS_BLOCKS: u32 = 2;
const FRAGMENTED_FULL_BLOCKS: u8 = 29;
const FRAGMENTED_BLOCKS: u32 = FRAGMENTED_FULL_BLOCKS as u32 + 1;
const FRAGMENTED_TRAILING_BYTES: &[u8] = b"unaligned trailing bytes";
const FIRST_FRAGMENT: AllocationBlock = AllocationBlock(734);
const FIRST_FRAGMENT_BLOCKS: u32 = 4;
const FIRST_CONTINUATION: u32 = 11;
const LAST_CONTINUATION: u32 = 27;
const CONTINUATION_BLOCKS: [u32; 3] = [FIRST_CONTINUATION, 19, LAST_CONTINUATION];
const FRAGMENT_COUNT: usize = 27;
const SAMPLE_BYTES: usize = 11;
const LEAF: NodeId = NodeId(2);
const LEAF_OFFSET: u64 = 155_648;
const CONTIGUOUS_RECORD: u64 = 155_744;
const FIRST_CONTINUATION_RECORD: u64 = 156_044;
const KEY_START_BLOCK_OFFSET: u64 = 8;
const RECORD_DESCRIPTOR_OFFSET: u64 = 8;
const FORK_BLOCK_COUNT_OFFSET: u64 = 12;
const FORK_EXTENTS_OFFSET: u64 = 16;
const FIRST_PAYLOAD: u64 = FIRST_FRAGMENT.0 as u64 * BLOCK_BYTES as u64;

#[test]
fn reads_native_contiguous_and_fragmented_attributes_exactly_and_on_demand() {
	let source = Slices::new(IMAGE);
	let volume = Volume::open(&source).unwrap();
	let attrs = volume.attributes(AttributeReadLimits::default()).unwrap();
	let entries = attrs.list_attrs(OWNER).unwrap();
	let expected = fragmented_value();
	let expected_len = ByteLen::from(expected.len());
	for (name, encoding) in [
		(INLINE, AttributeEncoding::Inline(ByteLen(5))),
		(
			CONTIGUOUS,
			AttributeEncoding::Fork(ByteLen::from(CONTIGUOUS_BYTES)),
		),
		(FRAGMENTED, AttributeEncoding::Fork(expected_len)),
	] {
		assert_eq!(
			entries
				.iter()
				.find(|entry| entry.name == units(name))
				.unwrap()
				.encoding,
			encoding
		);
	}
	let value = attrs
		.open_attribute(OWNER, &units(FRAGMENTED))
		.unwrap()
		.unwrap();
	assert_eq!(source.payload_reads.get(), 0);
	assert_eq!(value.len(), expected_len);
	assert_eq!(value.extents().count(), FRAGMENT_COUNT);
	assert_eq!(
		value.extents().map(|extent| extent.blocks).sum::<u32>(),
		FRAGMENTED_BLOCKS
	);
	assert_eq!(
		value.extents().next(),
		Some(Extent {
			start: FIRST_FRAGMENT,
			blocks: FIRST_FRAGMENT_BLOCKS
		})
	);
	for boundary in CONTINUATION_BLOCKS {
		let at = boundary as usize * BLOCK_BYTES - SAMPLE_BYTES;
		let mut sample = [0; SAMPLE_BYTES * 2];
		value
			.read_exact_at(ByteOffset(at as u64), &mut sample)
			.unwrap();
		assert_eq!(sample, expected[at..at + sample.len()]);
	}
	assert_eq!(
		attrs.open_attr(OWNER, &units(INLINE)).unwrap().unwrap(),
		b"short"
	);
	assert_eq!(
		attrs.open_attr(OWNER, &units(CONTIGUOUS)).unwrap().unwrap(),
		vec![b'0'; CONTIGUOUS_BYTES]
	);
	assert_eq!(
		attrs.open_attr(OWNER, &units(FRAGMENTED)).unwrap().unwrap(),
		expected
	);
	assert!(
		value
			.read_exact_at(ByteOffset(expected_len.0), &mut [])
			.is_ok()
	);
	assert!(matches!(
		value.read_exact_at(ByteOffset(expected_len.0), &mut [0]),
		Err(VolumeError::Bounds(RangeError::OutsideSource))
	));
}

#[test]
fn check_accounts_for_native_attribute_and_file_forks_without_reading_payloads() {
	let source = Slices::new(IMAGE).with_structure();
	let volume = Volume::open(&source).unwrap();
	let report = volume.check_structure(CheckLimits::default()).unwrap();
	assert_eq!(report.free_blocks, NATIVE_FREE_BLOCKS);
	assert_eq!(report.metadata_blocks, NATIVE_METADATA_BLOCKS);
	assert_eq!(report.reachable_fork_blocks, NATIVE_FILE_BLOCKS);
	assert_eq!(
		report.attribute_fork_blocks,
		CONTIGUOUS_BLOCKS + FRAGMENTED_BLOCKS
	);
	assert_eq!(report.unclaimed_allocated_blocks, 0);
	assert_eq!(
		report.attributes,
		AttributeCoverage::DeclaredOwners {
			inline: NATIVE_INLINE_NAMES,
			fork_backed: NATIVE_FORK_NAMES,
			unsupported: 0,
		}
	);
	let limits = CheckLimits {
		max_attribute_names: NATIVE_INLINE_NAMES + NATIVE_FORK_NAMES - 1,
		..CheckLimits::default()
	};
	assert!(matches!(
		volume.check_structure(limits),
		Err(CheckError::Limit(CheckLimit::AttributeNames))
	));
	assert_eq!(source.payload_reads.get(), 0);
}

#[test]
fn reachable_check_reads_native_attribute_forks_and_reports_a_later_byte_fault() {
	let mut source = Slices::new(IMAGE).with_structure();
	let limits = CheckLimits::default();
	let volume = Volume::open(&source).unwrap();
	let structure = volume.check_structure(limits).unwrap();
	let attributes = volume.attributes(AttributeReadLimits::default()).unwrap();
	let value = attributes
		.open_attribute(OWNER, &units(CONTIGUOUS))
		.unwrap()
		.unwrap();
	let extent = value.extents().next().unwrap();
	let base = volume
		.header()
		.geometry
		.block_range(extent.start, extent.blocks)
		.unwrap()
		.start;

	// Catalog and attribute metadata and both attributes are native bytes.
	// The fixture omits ballast payloads, so give those bytes a readable backing.
	source.fill_unrecorded = true;
	let max_payload_bytes = source.len();
	let volume = Volume::open(&source).unwrap();
	let report = volume
		.check_reachable_data(limits, max_payload_bytes)
		.unwrap();
	assert_eq!(report.structure, structure);
	assert_eq!(report.attribute_forks, NATIVE_FORK_NAMES as u64);
	assert!(report.stored_bytes.0 > (CONTIGUOUS_BYTES + fragmented_value().len()) as u64);
	assert_eq!(
		volume
			.check_reachable_data(limits, report.stored_bytes)
			.unwrap(),
		report
	);

	source.fail_at = Some(base + BLOCK_BYTES as u64);
	let volume = Volume::open(&source).unwrap();
	assert_eq!(volume.check_structure(limits).unwrap(), structure);
	assert!(matches!(
		volume.check_reachable_data(limits, max_payload_bytes),
		Err(CheckError::PayloadRead {
			owner: AllocationOwner::AttributeFork { id: OWNER, ordinal: CONTIGUOUS_ORDINAL },
			offset: ByteOffset(offset),
			error: VolumeError::Source { error: RangeError::OutsideSource, .. },
		}) if offset == BLOCK_BYTES as u64
	));
}

#[test]
fn check_rejects_an_attribute_fork_that_claims_a_file_data_block() {
	let mut image = IMAGE.to_vec();
	let at =
		body_offset(CONTIGUOUS_RECORD, CONTIGUOUS) + RECORD_DESCRIPTOR_OFFSET + FORK_EXTENTS_OFFSET;
	patch(&mut image, at, &SAMPLE_DATA_BLOCK.0.to_be_bytes());
	let source = Slices::new(&image).with_structure();
	let volume = Volume::open(&source).unwrap();
	let attribute = AllocationOwner::AttributeFork {
		id: OWNER,
		ordinal: CONTIGUOUS_ORDINAL,
	};
	let file = AllocationOwner::FileFork {
		id: OWNER,
		kind: ForkKind::Data,
	};
	assert!(matches!(
		volume.check_structure(CheckLimits::default()),
		Err(CheckError::Allocation(AllocationError::Overlap {
			block: SAMPLE_DATA_BLOCK,
			first,
			second,
		})) if [first, second].contains(&attribute) && [first, second].contains(&file)
	));
	assert_eq!(source.payload_reads.get(), 0);
}

#[test]
fn check_reports_compression_metadata_as_unsupported() {
	let source = Slices::new(IMAGE);
	let raw = source
		.segment(LEAF_OFFSET, usize::from(NATIVE_NODE_BYTES))
		.unwrap();
	let node = Node::decode(raw).unwrap();
	let raw_records: Vec<_> = node.records().collect();
	let ordinal = raw_records
		.iter()
		.position(|raw| Key::split(raw).unwrap().0.file_id == BALLAST_OWNER)
		.unwrap();
	let (key, body) = Key::split(raw_records[ordinal]).unwrap();
	let name = units(COMPRESSED_ATTRIBUTE_NAME);
	let key_len = Key::encoded_len(&name).unwrap();
	let mut compressed = vec![0; key_len + body.len()];
	Key::encode(key.file_id, &name, key.start_block, &mut compressed).unwrap();
	compressed[key_len..].copy_from_slice(body);
	let mut records = raw_records;
	records[ordinal] = &compressed;
	let links = NodeLinks {
		forward: node.forward(),
		backward: node.backward(),
	};
	let mut leaf = vec![0; usize::from(NATIVE_NODE_BYTES)];
	Node::encode(&mut leaf, Kind::Leaf, LEAF_HEIGHT, links, &records).unwrap();
	let mut image = IMAGE.to_vec();
	patch(&mut image, LEAF_OFFSET, &leaf);
	let volume = Volume::open(Slices::new(&image).with_structure()).unwrap();
	let report = volume.check_structure(CheckLimits::default()).unwrap();
	assert_eq!(
		report.attributes,
		AttributeCoverage::DeclaredOwners {
			inline: NATIVE_INLINE_NAMES - 1,
			fork_backed: NATIVE_FORK_NAMES,
			unsupported: 1,
		}
	);
	assert_eq!(report.unclaimed_allocated_blocks, 0);
}

#[test]
fn rejects_continuations_with_gaps_or_overlapping_physical_extents() {
	let mut image = IMAGE.to_vec();
	let start = FIRST_CONTINUATION_RECORD + KEY_START_BLOCK_OFFSET;
	let wrong = FIRST_CONTINUATION + 1;
	patch(&mut image, start, &wrong.to_be_bytes());
	let volume = Volume::open(Slices::new(&image)).unwrap();
	let attrs = volume.attributes(AttributeReadLimits::default()).unwrap();
	assert!(
		matches!(attrs.open_attribute(OWNER, &units(FRAGMENTED)), Err(AttributeReadError::InvalidStart { node: LEAF, expected: FIRST_CONTINUATION, actual }) if actual == wrong)
	);
	assert!(
		matches!(attrs.list_attrs(OWNER), Err(AttributeReadError::InvalidStart { node: LEAF, expected: FIRST_CONTINUATION, actual }) if actual == wrong)
	);
	let structure = Volume::open(Slices::new(&image).with_structure()).unwrap();
	assert!(matches!(
		structure.check_structure(CheckLimits::default()),
		Err(CheckError::Attributes(AttributeReadError::InvalidStart {
			node: LEAF,
			expected: FIRST_CONTINUATION,
			actual,
		})) if actual == wrong
	));

	let mut image = IMAGE.to_vec();
	let at = body_offset(FIRST_CONTINUATION_RECORD, FRAGMENTED) + RECORD_DESCRIPTOR_OFFSET;
	patch(&mut image, at, &FIRST_FRAGMENT.0.to_be_bytes());
	let volume = Volume::open(Slices::new(&image)).unwrap();
	let attrs = volume.attributes(AttributeReadLimits::default()).unwrap();
	assert!(matches!(
		attrs.open_attribute(OWNER, &units(FRAGMENTED)),
		Err(AttributeReadError::Layout {
			node: LEAF,
			error: OverflowError::Read(VolumeError::Format(FormatError::InvalidFork))
		})
	));
}

#[test]
fn refuses_a_truncated_chain_and_reserved_blocks_before_payload_reads() {
	let source = Slices::new(IMAGE);
	let raw = source
		.segment(LEAF_OFFSET, usize::from(NATIVE_NODE_BYTES))
		.unwrap();
	let node = Node::decode(raw).unwrap();
	let records: Vec<_> = node
		.records()
		.filter(|record| {
			let key = Key::split(record).unwrap().0;
			key.file_id != OWNER
				|| key.start_block != LAST_CONTINUATION
				|| key.name.units().ne(FRAGMENTED.encode_utf16())
		})
		.collect();
	let links = NodeLinks {
		forward: node.forward(),
		backward: node.backward(),
	};
	let mut leaf = vec![0; usize::from(NATIVE_NODE_BYTES)];
	Node::encode(&mut leaf, Kind::Leaf, LEAF_HEIGHT, links, &records).unwrap();
	let mut image = IMAGE.to_vec();
	patch(&mut image, LEAF_OFFSET, &leaf);
	let volume = Volume::open(Slices::new(&image)).unwrap();
	let attrs = volume.attributes(AttributeReadLimits::default()).unwrap();
	assert!(matches!(
		attrs.open_attribute(OWNER, &units(FRAGMENTED)),
		Err(AttributeReadError::IncompleteFork {
			node: LEAF,
			covered: LAST_CONTINUATION,
			declared: FRAGMENTED_BLOCKS
		})
	));

	let mut image = IMAGE.to_vec();
	let at =
		body_offset(CONTIGUOUS_RECORD, CONTIGUOUS) + RECORD_DESCRIPTOR_OFFSET + FORK_EXTENTS_OFFSET;
	patch(&mut image, at, &0u32.to_be_bytes());
	let volume = Volume::open(Slices::new(&image)).unwrap();
	let attrs = volume.attributes(AttributeReadLimits::default()).unwrap();
	assert!(matches!(
		attrs.open_attr(OWNER, &units(CONTIGUOUS)),
		Err(AttributeReadError::Layout {
			node: LEAF,
			error: OverflowError::Read(VolumeError::Format(FormatError::InvalidFork))
		})
	));
}

#[test]
fn limits_owned_reads_and_keeps_the_physical_error_offset() {
	let mut image = IMAGE.to_vec();
	let size = MAX_OWNED_ATTRIBUTE_BYTES.0 + 1;
	let blocks = u32::try_from(size.div_ceil(BLOCK_BYTES as u64)).unwrap();
	let fork = body_offset(CONTIGUOUS_RECORD, CONTIGUOUS) + RECORD_DESCRIPTOR_OFFSET;
	patch(&mut image, fork, &size.to_be_bytes());
	patch(
		&mut image,
		fork + FORK_BLOCK_COUNT_OFFSET,
		&blocks.to_be_bytes(),
	);
	patch(
		&mut image,
		fork + FORK_EXTENTS_OFFSET + size_of::<u32>() as u64,
		&blocks.to_be_bytes(),
	);
	let source = Slices::new(&image);
	let volume = Volume::open(&source).unwrap();
	let attrs = volume.attributes(AttributeReadLimits::default()).unwrap();
	assert!(
		matches!(attrs.open_attr(OWNER, &units(CONTIGUOUS)), Err(AttributeReadError::ValueLimit { bytes }) if bytes.0 == size)
	);
	assert_eq!(
		attrs
			.open_attribute(OWNER, &units(CONTIGUOUS))
			.unwrap()
			.unwrap()
			.len(),
		ByteLen(size)
	);
	assert_eq!(source.payload_reads.get(), 0);

	let mut source = Slices::new(IMAGE);
	source.fail_at = Some(FIRST_PAYLOAD);
	let volume = Volume::open(source).unwrap();
	let attrs = volume.attributes(AttributeReadLimits::default()).unwrap();
	let value = attrs
		.open_attribute(OWNER, &units(FRAGMENTED))
		.unwrap()
		.unwrap();
	assert!(
		matches!(value.read_exact_at(ByteOffset(0), &mut [0]), Err(VolumeError::Source { offset, error: RangeError::OutsideSource }) if offset.0 == FIRST_PAYLOAD)
	);
}

fn fragmented_value() -> Vec<u8> {
	(1..=FRAGMENTED_FULL_BLOCKS)
		.flat_map(|value| std::iter::repeat_n(value, BLOCK_BYTES))
		.chain(FRAGMENTED_TRAILING_BYTES.iter().copied())
		.collect()
}

fn units(name: &str) -> Vec<u16> {
	name.encode_utf16().collect()
}

fn body_offset(record: u64, name: &str) -> u64 {
	record + Key::encoded_len(&units(name)).unwrap() as u64
}

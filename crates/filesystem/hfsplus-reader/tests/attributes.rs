//! Native two-level attribute traversal evidence for H-READ-01-T01.

use std::cell::Cell;

use hfsplus_core::{ByteLen, ByteOffset, ByteRange, ReadAt};
use hfsplus_format::{
	Error as FormatError,
	attributes::{Key, NATIVE_INLINE_MAX_VALUE_BYTES, NATIVE_NODE_BYTES},
	btree::NodeId,
	volume::{CatalogId, HEADER_BYTES, PRIMARY_HEADER_OFFSET},
};
use hfsplus_reader::{
	Error as VolumeError, Volume,
	attributes::{AttributeEncoding, AttributeReadError, AttributeReadLimits},
};

const NATIVE: &[u8] = include_bytes!("fixtures/native-attribute-tree.bin");
const OPTICAL: &[u8] = include_bytes!("fixtures/optical-volume.bin");
const ORIGINAL_BYTES: u64 = 1 << 24;
const ALTERNATE_OFFSET: u64 = ORIGINAL_BYTES - 1024;
const TREE_SOURCE_OFFSET: u64 = 139_264;
const TREE_FIXTURE_OFFSET: usize = HEADER_BYTES * 2;
const NODE_BYTES: usize = NATIVE_NODE_BYTES as usize;
const ROOT_NODE: NodeId = NodeId(3);
const FIRST_LEAF: NodeId = NodeId(2);
const LAST_LEAF: NodeId = NodeId(1);
const OWNER: CatalogId = CatalogId(18);
const ABSENT_OWNER: CatalogId = CatalogId(19);
const PROVENANCE: &str = "com.apple.provenance";
const EMPTY: &str = "org.pomologie.empty";
const DECOMPOSED: &str = "org.pomologie.e\u{301}";
const COMPOSED: &str = "org.pomologie.é";
const BELOW_LIMIT: &str = "org.pomologie.n3801";
const INLINE_LIMIT: &str = "org.pomologie.n3802";
const FIRST_FORK: &str = "org.pomologie.n3803";
const SECOND_FORK: &str = "org.pomologie.n4096";
const FORK_RECORD: u32 = 0x20;
const INLINE_FILL_BYTE: u8 = b's';

#[test]
fn lists_native_attributes_and_reads_exact_distinct_inline_values() {
	let source = SparseVolume::new(NATIVE);
	let volume = Volume::open(&source).unwrap();
	let attributes = volume.attributes(AttributeReadLimits::default()).unwrap();
	let entries = attributes.list_attrs(OWNER).unwrap();
	let names: Vec<_> = entries
		.iter()
		.map(|entry| String::from_utf16(&entry.name).unwrap())
		.collect();
	assert_eq!(
		names,
		[
			PROVENANCE,
			EMPTY,
			DECOMPOSED,
			BELOW_LIMIT,
			INLINE_LIMIT,
			FIRST_FORK,
			SECOND_FORK,
			COMPOSED
		]
	);
	let encoding =
		|name: &str| entries[names.iter().position(|actual| actual == name).unwrap()].encoding;
	assert_eq!(encoding(EMPTY), AttributeEncoding::Inline(ByteLen(0)));
	assert_eq!(
		encoding(INLINE_LIMIT),
		AttributeEncoding::Inline(ByteLen::from(NATIVE_INLINE_MAX_VALUE_BYTES))
	);
	assert_eq!(
		encoding(FIRST_FORK),
		AttributeEncoding::UnsupportedRecord(FORK_RECORD)
	);
	assert_eq!(
		encoding(SECOND_FORK),
		AttributeEncoding::UnsupportedRecord(FORK_RECORD)
	);
	assert_eq!(source.node_reads.get(), 4);

	let read = |name: &str| attributes.open_attr(OWNER, &utf16(name)).unwrap();
	assert_eq!(read(EMPTY), Some(vec![]));
	assert_eq!(read(DECOMPOSED), Some(vec![2]));
	assert_eq!(read(COMPOSED), Some(vec![1]));
	assert_eq!(
		read(INLINE_LIMIT),
		Some(vec![INLINE_FILL_BYTE; NATIVE_INLINE_MAX_VALUE_BYTES])
	);
	assert_eq!(read("org.pomologie.absent"), None);
	assert!(attributes.list_attrs(ABSENT_OWNER).unwrap().is_empty());

	assert!(matches!(
		attributes.open_attr(OWNER, &utf16(FIRST_FORK)),
		Err(AttributeReadError::Format {
			node: LAST_LEAF,
			error: FormatError::UnsupportedAttributeRecord(FORK_RECORD),
		})
	));
}

#[test]
fn leaves_large_forks_unread_and_meets_node_and_entry_budgets() {
	let source = SparseVolume::new(NATIVE);
	let volume = Volume::open(&source).unwrap();
	let limits = AttributeReadLimits {
		max_nodes: 2,
		max_entries: 10,
	};
	let attributes = volume.attributes(limits).unwrap();
	assert_eq!(
		attributes.open_attr(OWNER, &utf16(DECOMPOSED)).unwrap(),
		Some(vec![2])
	);
	assert_eq!(source.node_reads.get(), 3);
	assert!(matches!(
		attributes.list_attrs(OWNER),
		Err(AttributeReadError::NodeLimit)
	));
	assert_eq!(source.node_reads.get(), 5);
	assert!(matches!(
		attributes.open_attr(OWNER, &[]),
		Err(AttributeReadError::InvalidName)
	));
	assert_eq!(source.node_reads.get(), 5);

	let attributes = volume
		.attributes(AttributeReadLimits {
			max_nodes: 3,
			max_entries: 2,
		})
		.unwrap();
	assert!(matches!(
		attributes.list_attrs(OWNER),
		Err(AttributeReadError::EntryLimit)
	));
}

#[test]
fn empty_attributes_fork_returns_no_entries_and_no_values() {
	let volume = Volume::open(OPTICAL).unwrap();
	let attributes = volume.attributes(AttributeReadLimits::default()).unwrap();
	assert!(attributes.list_attrs(OWNER).unwrap().is_empty());
	assert_eq!(attributes.open_attr(OWNER, &utf16(EMPTY)).unwrap(), None);
}

#[test]
fn rejects_index_children_outside_the_allocation_map() {
	const NODE_DESCRIPTOR_BYTES: usize = 14;
	const FREE_NODE_ID: u32 = 4;
	let mut fixture = NATIVE.to_vec();
	let offset = fixture_offset(ROOT_NODE)
		+ NODE_DESCRIPTOR_BYTES
		+ Key::encoded_len(&utf16(PROVENANCE)).unwrap();
	fixture[offset..][..size_of::<u32>()].copy_from_slice(&FREE_NODE_ID.to_be_bytes());
	let volume = Volume::open(SparseVolume::new(&fixture)).unwrap();
	let attributes = volume.attributes(AttributeReadLimits::default()).unwrap();
	assert!(matches!(
		attributes.list_attrs(OWNER),
		Err(AttributeReadError::Format {
			node: ROOT_NODE,
			error: FormatError::InvalidNode
		})
	));
}

#[test]
fn rejects_broken_forward_and_backward_leaf_relationships() {
	const NULL_LINK: u32 = 0;
	const NODE_BACKWARD_LINK_OFFSET: usize = size_of::<u32>();
	for (node_id, link_offset) in [(FIRST_LEAF, 0), (LAST_LEAF, NODE_BACKWARD_LINK_OFFSET)] {
		let mut fixture = NATIVE.to_vec();
		let offset = fixture_offset(node_id) + link_offset;
		fixture[offset..][..size_of::<u32>()].copy_from_slice(&NULL_LINK.to_be_bytes());
		let volume = Volume::open(SparseVolume::new(&fixture)).unwrap();
		let attributes = volume.attributes(AttributeReadLimits::default()).unwrap();
		assert!(matches!(
			attributes.list_attrs(OWNER),
			Err(AttributeReadError::Format { node, error: FormatError::InvalidNode }) if node == node_id
		));
	}
}

#[test]
fn retains_failed_source_offset_and_cause() {
	let mut source = SparseVolume::new(NATIVE);
	source.failing_node = Some(LAST_LEAF);
	let volume = Volume::open(source).unwrap();
	let attributes = volume.attributes(AttributeReadLimits::default()).unwrap();
	let expected = ByteOffset(TREE_SOURCE_OFFSET + u64::from(NATIVE_NODE_BYTES));
	assert!(
		matches!(attributes.list_attrs(OWNER), Err(AttributeReadError::Fork(VolumeError::Source {
		offset, error: FixtureError::Rejected,
	})) if offset == expected)
	);
}

fn utf16(text: &str) -> Vec<u16> {
	text.encode_utf16().collect()
}

fn fixture_offset(id: NodeId) -> usize {
	TREE_FIXTURE_OFFSET + id.0 as usize * NODE_BYTES
}

struct SparseVolume<'fixture> {
	fixture: &'fixture [u8],
	node_reads: Cell<usize>,
	failing_node: Option<NodeId>,
}

impl<'fixture> SparseVolume<'fixture> {
	fn new(fixture: &'fixture [u8]) -> Self {
		Self {
			fixture,
			node_reads: Cell::new(0),
			failing_node: None,
		}
	}
}

#[derive(Debug, PartialEq, Eq)]
enum FixtureError {
	OutsideStoredSegments,
	Rejected,
}

impl ReadAt for SparseVolume<'_> {
	type Error = FixtureError;

	fn len(&self) -> ByteLen {
		ByteLen(ORIGINAL_BYTES)
	}

	fn read_exact_at(&self, offset: ByteOffset, destination: &mut [u8]) -> Result<(), Self::Error> {
		let range = ByteRange::new(offset, ByteLen::from(destination.len()), self.len())
			.map_err(|_| FixtureError::OutsideStoredSegments)?;
		if destination.len() == NODE_BYTES && offset.0 >= TREE_SOURCE_OFFSET {
			self.node_reads.set(self.node_reads.get() + 1);
			let node =
				NodeId(((offset.0 - TREE_SOURCE_OFFSET) / u64::from(NATIVE_NODE_BYTES)) as u32);
			if self.failing_node == Some(node) {
				return Err(FixtureError::Rejected);
			}
		}
		let (start, end) = (range.start().0, range.end().0);
		for (base, bytes) in [
			(PRIMARY_HEADER_OFFSET, &self.fixture[..HEADER_BYTES]),
			(
				ALTERNATE_OFFSET,
				&self.fixture[HEADER_BYTES..TREE_FIXTURE_OFFSET],
			),
			(TREE_SOURCE_OFFSET, &self.fixture[TREE_FIXTURE_OFFSET..]),
		] {
			if start >= base && end <= base + bytes.len() as u64 {
				destination
					.copy_from_slice(&bytes[((start - base) as usize)..((end - base) as usize)]);
				return Ok(());
			}
		}
		Err(FixtureError::OutsideStoredSegments)
	}
}

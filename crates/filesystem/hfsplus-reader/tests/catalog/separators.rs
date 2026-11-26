//! Exact separator routing through the public catalog lookup APIs.

use std::{
	cell::RefCell,
	collections::{BTreeMap, BTreeSet},
};

use hfsplus_core::{ByteLen, ByteOffset, RangeError, ReadAt};
use hfsplus_format::{
	btree::{Kind, Node, NodeId},
	catalog::{Key, Metadata, Record, Value},
	volume::CatalogId,
};
use proptest::{
	prelude::*,
	test_runner::{RngSeed, TestCaseError, TestRunner},
};

use super::{
	CatalogLimits, EMPTY_FILES, Entry, EntryKind, Layout, NATIVE, NATIVE_NODE_BYTES, ROOT_ID,
	Volume, layout,
};

const NATIVE_DEPTH: u16 = 2;
const CASES: u32 = 1024;
const SEED: u64 = 0x5345_5041;
const ABSENT_PROBES: usize = 16;

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd)]
struct RawKey(CatalogId, Vec<u16>);

impl From<Key<'_>> for RawKey {
	fn from(key: Key<'_>) -> Self {
		Self(key.parent_or_thread, key.name.units().collect())
	}
}

struct LeafRecord {
	node: NodeId,
	ordinal: usize,
	entry: Option<ExpectedEntry>,
}

#[derive(Debug, Eq, PartialEq)]
struct ExpectedEntry {
	parent: CatalogId,
	name: Vec<u16>,
	metadata: Metadata,
	kind: EntryKind,
}

impl From<Entry> for ExpectedEntry {
	fn from(entry: Entry) -> Self {
		Self {
			parent: entry.parent,
			name: entry.name,
			metadata: entry.metadata,
			kind: entry.kind,
		}
	}
}

#[derive(Default)]
struct TracedSource(RefCell<Vec<ByteOffset>>);

impl ReadAt for TracedSource {
	type Error = RangeError;

	fn len(&self) -> ByteLen {
		ReadAt::len(NATIVE)
	}

	fn read_exact_at(&self, offset: ByteOffset, destination: &mut [u8]) -> Result<(), Self::Error> {
		self.0.borrow_mut().push(offset);
		NATIVE.read_exact_at(offset, destination)
	}
}

fn node_offset(layout: &Layout, node: NodeId) -> usize {
	layout.base + usize::try_from(node.0).unwrap() * NATIVE_NODE_BYTES
}

fn read_node(layout: &Layout, node: NodeId) -> Node<'static> {
	let offset = node_offset(layout, node);
	Node::decode(&NATIVE[offset..offset + NATIVE_NODE_BYTES]).unwrap()
}

fn reference_leaf_records(layout: &Layout) -> BTreeMap<RawKey, LeafRecord> {
	let mut records = BTreeMap::new();
	let mut visited = BTreeSet::new();
	let mut next = layout.header.first_leaf;
	while let Some(id) = next {
		assert!(visited.insert(id));
		let leaf = read_node(layout, id);
		assert_eq!(leaf.kind(), Kind::Leaf);
		for (ordinal, raw) in leaf.records().enumerate() {
			let record = Record::decode(raw, layout.volume.geometry).unwrap();
			let key = RawKey::from(record.key);
			let to_entry = |metadata, kind| ExpectedEntry {
				parent: key.0,
				name: key.1.clone(),
				metadata,
				kind,
			};
			let entry = match record.value {
				Value::Folder {
					metadata,
					valence,
					tail_word,
				} => Some(to_entry(
					metadata,
					EntryKind::Directory { valence, tail_word },
				)),
				Value::File {
					metadata,
					data,
					resource,
				} => Some(to_entry(metadata, EntryKind::File { data, resource })),
				Value::Thread { .. } => None,
			};
			assert!(
				records
					.insert(
						key,
						LeafRecord {
							node: id,
							ordinal,
							entry
						}
					)
					.is_none()
			);
		}
		next = leaf.forward();
	}
	assert_eq!(
		records.len(),
		usize::try_from(layout.header.leaf_records).unwrap()
	);
	records
}

#[test]
fn exact_index_separators_follow_the_first_leaf_record() {
	let layout = layout(NATIVE);
	assert_eq!(layout.header.depth, NATIVE_DEPTH);
	let reference = reference_leaf_records(&layout);
	let by_id: BTreeMap<_, _> = reference
		.values()
		.filter_map(|record| record.entry.as_ref())
		.map(|entry| (entry.metadata.id, entry))
		.collect();
	let root = read_node(&layout, layout.header.root.unwrap());
	assert_eq!(root.kind(), Kind::Index);
	let boundaries: Vec<_> = root
		.records()
		.map(|raw| {
			let (key, pointer) = Key::split_leaf(raw).unwrap();
			let child = NodeId(u32::from_be_bytes(pointer.try_into().unwrap()));
			(RawKey::from(key), child)
		})
		.collect();
	assert_eq!(
		boundaries.len(),
		reference
			.values()
			.filter(|record| record.ordinal == 0)
			.count()
	);
	assert!(boundaries.len() > 1);

	let source = TracedSource::default();
	let volume = Volume::open(&source).unwrap();
	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	let check_exact = |(key, child): &(RawKey, NodeId)| -> Result<(), TestCaseError> {
		let record = &reference[key];
		prop_assert_eq!(record.ordinal, 0);
		prop_assert_eq!(record.node, *child);
		source.0.borrow_mut().clear();
		let (actual, expected) = match &record.entry {
			Some(entry) if entry.metadata.id == ROOT_ID => (catalog.root().map(Some), entry),
			Some(entry) => (catalog.lookup_raw(key.0, &key.1), entry),
			None => (catalog.entry(key.0), by_id[&key.0]),
		};
		let actual = actual
			.map_err(|error| TestCaseError::fail(error.to_string()))?
			.map(ExpectedEntry::from);
		prop_assert_eq!(actual.as_ref(), Some(expected));
		let expected_offset = ByteOffset(u64::try_from(node_offset(&layout, record.node)).unwrap());
		prop_assert!(source.0.borrow().contains(&expected_offset));
		Ok(())
	};
	for boundary in &boundaries {
		check_exact(boundary).unwrap();
	}
	println!(
		"verified {} equal separators over {} leaf records",
		boundaries.len(),
		reference.len()
	);

	let mut runner = TestRunner::new(ProptestConfig {
		cases: CASES,
		rng_seed: RngSeed::Fixed(SEED),
		failure_persistence: None,
		..ProptestConfig::default()
	});
	runner
		.run(
			&(
				0..boundaries.len(),
				0..EMPTY_FILES + ABSENT_PROBES,
				any::<bool>(),
			),
			|(boundary, index, uppercase)| {
				check_exact(&boundaries[boundary])?;
				let name = format!("item-{index:03}");
				let expected = reference
					.get(&RawKey(ROOT_ID, name.encode_utf16().collect()))
					.and_then(|record| record.entry.as_ref());
				let query = if uppercase {
					name.to_ascii_uppercase()
				} else {
					name
				};
				let actual = catalog
					.lookup(ROOT_ID, &query)
					.map_err(|error| TestCaseError::fail(error.to_string()))?
					.map(ExpectedEntry::from);
				prop_assert_eq!(actual.as_ref(), expected, "query {:?}", query);
				Ok(())
			},
		)
		.unwrap();
}

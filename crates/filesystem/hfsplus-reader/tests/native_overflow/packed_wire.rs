use std::collections::BTreeMap;

use hfsplus_format::{
	attributes,
	btree::{Kind, Node},
	catalog::{self, Metadata, ThreadKind, Value},
	extents::{EXTENTS_PER_RECORD, Key as ExtentKey},
	volume::{Extent, Fork, Geometry, Header},
};
use hfsplus_reader::{
	Volume,
	overflow::{ForkKind, OverflowLimits},
};

use super::{NATIVE, NATIVE_RECORD_INDEX, Slices};

const CATALOG: &[u8] = include_bytes!("../fixtures/native-catalog-volume.bin");
const ATTRIBUTES: &[u8] = include_bytes!("../fixtures/native-attribute-tree.bin");
const CATALOG_HEADER_START: usize = 1024;
const CATALOG_TREE_START: usize = 2048;
const CATALOG_NODE_BYTES: usize = 4096;
const CATALOG_NODES: usize = 18;
const ATTRIBUTE_VOLUME_BYTES: u64 = 16_777_216;
const ATTRIBUTE_TREE_START: usize = 1024;
const ATTRIBUTE_NODE_BYTES: usize = 8192;
const EXTENT_LEAF_SLICE: usize = 3;
const EXTENT_LEAF_NODE: usize = 1;

#[test]
fn reports_packed_native_records_for_the_wire_oracle() {
	let geometry = Header::decode(&CATALOG[CATALOG_HEADER_START..], CATALOG.len() as u64)
		.unwrap()
		.geometry;
	let catalog_end = CATALOG_TREE_START + CATALOG_NODES * CATALOG_NODE_BYTES;
	for (id, bytes) in CATALOG[CATALOG_TREE_START..catalog_end]
		.as_chunks::<CATALOG_NODE_BYTES>()
		.0
		.iter()
		.enumerate()
		.skip(1)
	{
		report_node("catalog", id, bytes, |kind, raw| {
			catalog_record(kind, raw, geometry)
		});
	}
	let source = Slices::new(NATIVE);
	let bytes = source.slices[EXTENT_LEAF_SLICE].1;
	let groups = resolved_extent_groups(source);
	report_node("extents", EXTENT_LEAF_NODE, bytes, |kind, raw| {
		assert_eq!(kind, Kind::Leaf);
		let key = ExtentKey::decode(raw).unwrap();
		let fork = key.kind as u8;
		let start = key.start_block;
		format!(
			"{}|{fork}|{start}|{:?}",
			key.id.0,
			extent_words(&groups[&(fork, start)])
		)
	});
	let geometry = Header::decode(ATTRIBUTES, ATTRIBUTE_VOLUME_BYTES)
		.unwrap()
		.geometry;
	for (id, bytes) in ATTRIBUTES[ATTRIBUTE_TREE_START..]
		.as_chunks::<ATTRIBUTE_NODE_BYTES>()
		.0
		.iter()
		.enumerate()
		.skip(1)
	{
		report_node("attributes", id, bytes, |kind, raw| {
			attribute_record(kind, raw, geometry)
		});
	}
}

fn report_node(tree: &str, id: usize, bytes: &[u8], value: impl Fn(Kind, &[u8]) -> String) {
	let node = Node::decode(bytes).unwrap();
	let kind = match node.kind() {
		Kind::Leaf => "leaf",
		Kind::Index => "index",
		other => panic!("unexpected {tree} node {id} kind {other:?}"),
	};
	println!(
		"HFS+ node {tree}|{id}|{}|{kind}|{}|{}|{}|{}",
		bytes.len(),
		node.height(),
		node.forward().map_or(0, |link| link.0),
		node.backward().map_or(0, |link| link.0),
		node.records().count(),
	);
	for (ordinal, record) in node.records().enumerate() {
		let start = record.as_ptr().addr() - bytes.as_ptr().addr();
		let end = start + record.len();
		println!(
			"HFS+ record {tree}|{id}|{ordinal}|{start}|{end}|{}",
			value(node.kind(), record)
		);
	}
}

fn catalog_record(kind: Kind, raw: &[u8], geometry: Geometry) -> String {
	let (key, body) = catalog::Key::split_leaf(raw).unwrap();
	let name: Vec<_> = key.name.units().collect();
	let value = if kind == Kind::Index {
		format!("child|{}", u32::from_be_bytes(body.try_into().unwrap()))
	} else {
		match catalog::Record::decode(raw, geometry).unwrap().value {
			Value::Folder {
				metadata,
				valence,
				tail_word,
			} => {
				format!("folder|{}|{valence}|{tail_word}", report_metadata(metadata))
			}
			Value::File {
				metadata,
				data,
				resource,
			} => {
				format!(
					"file|{}|{:?}|{:?}",
					report_metadata(metadata),
					fork_words(&data),
					fork_words(&resource)
				)
			}
			Value::Thread { kind, parent, name } => {
				let kind = match kind {
					ThreadKind::Folder => "folder-thread",
					ThreadKind::File => "file-thread",
				};
				format!("{kind}|{}|{:?}", parent.0, name.units().collect::<Vec<_>>())
			}
		}
	};
	format!("{}|{name:?}|{value}", key.parent_or_thread.0)
}

fn report_metadata(metadata: Metadata) -> String {
	let permissions = metadata.permissions;
	format!(
		"{}|{}|{:?}|{}|{}|{}|{}|{}|{}|{:?}|{}",
		metadata.id.0,
		metadata.flags,
		metadata.dates,
		permissions.uid,
		permissions.gid,
		permissions.admin_flags,
		permissions.owner_flags,
		permissions.mode,
		permissions.special,
		metadata.finder_info,
		metadata.encoding,
	)
}

fn attribute_record(kind: Kind, raw: &[u8], geometry: Geometry) -> String {
	let (key, body) = attributes::Key::split(raw).unwrap();
	let name: Vec<_> = key.name.units().collect();
	let value = if kind == Kind::Index {
		format!("child|{}", u32::from_be_bytes(body.try_into().unwrap()))
	} else {
		match attributes::Record::decode(raw, geometry).unwrap().value {
			attributes::Value::Inline(value) => format!("inline|{value:?}"),
			attributes::Value::Fork(fork) => format!("fork|{:?}", fork_words(&fork)),
			other => panic!("unexpected attribute value {other:?}"),
		}
	};
	format!("{}|{name:?}|{}|{value}", key.file_id.0, key.start_block)
}

fn resolved_extent_groups(source: Slices<'_>) -> BTreeMap<(u8, u32), Vec<Extent>> {
	let raw = source.slices[NATIVE_RECORD_INDEX].1;
	let volume = Volume::open(source).unwrap();
	let record = catalog::Record::decode(raw, volume.header().geometry).unwrap();
	let Value::File {
		metadata,
		data,
		resource,
	} = record.value
	else {
		panic!("expected the native fragmented regular file");
	};
	let mut groups = BTreeMap::new();
	for (kind, fork) in [(ForkKind::Data, data), (ForkKind::Resource, resource)] {
		let resolved = volume
			.open_fork(metadata.id, kind, &fork, OverflowLimits::default())
			.unwrap();
		let embedded = fork.extents().count();
		let mut logical = fork.extents().map(|extent| extent.blocks).sum();
		let extents: Vec<_> = resolved.extents().skip(embedded).collect();
		for group in extents.chunks(EXTENTS_PER_RECORD) {
			assert!(
				groups
					.insert((kind as u8, logical), group.to_vec())
					.is_none()
			);
			logical += group.iter().map(|extent| extent.blocks).sum::<u32>();
		}
	}
	groups
}

fn fork_words(fork: &Fork) -> Vec<u64> {
	let mut words = vec![
		fork.logical_bytes,
		fork.clump_bytes.into(),
		fork.total_blocks.into(),
	];
	words.extend(
		fork.extents()
			.flat_map(|extent| [u64::from(extent.start.0), u64::from(extent.blocks)]),
	);
	words
}

fn extent_words(extents: &[Extent]) -> Vec<u32> {
	extents
		.iter()
		.flat_map(|extent| [extent.start.0, extent.blocks])
		.collect()
}

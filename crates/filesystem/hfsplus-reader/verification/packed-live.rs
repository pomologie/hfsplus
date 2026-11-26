use std::{env, fmt::Debug, path::PathBuf};

use hfsplus_core::{ByteOffset, ReadAt};
use hfsplus_format::btree::{Kind, Node, TreeHeader};
use hfsplus_reader::{SpecialFile, Volume, overflow::OverflowLimits};

#[path = "../tests/catalog/hosted.rs"]
mod hosted;

use super::{Cases, attribute_key, catalog_key, check_node, compare_catalog, extent_key};

const WITNESS_ENV: &str = "POMOLOGIE_FULL_FORK_WITNESS";
const RAW_IMAGE: &str = "native.cdr";
const NATIVE_VOLUME_BYTES: u64 = 64 << 20;
const MAX_LEAVES: usize = 32;
const MIN_CATALOG_RECORDS: usize = 10;
const MIN_EXTENT_RECORDS: usize = 4;
const MIN_ATTRIBUTE_RECORDS: usize = 2;

#[test]
#[ignore = "requires the bounded disposable Apple native fork and metadata witness"]
fn fresh_native_leaf_chains_match_packed_records() {
	let directory = PathBuf::from(env::var_os(WITNESS_ENV).unwrap());
	let raw = hosted::FixtureFile::open(&directory.join(RAW_IMAGE)).unwrap();
	assert_eq!(raw.len().0, NATIVE_VOLUME_BYTES);
	let volume = Volume::open(raw).unwrap();
	let geometry = volume.header().geometry;
	for (label, file, minimum_records) in [
		("catalog", SpecialFile::Catalog, MIN_CATALOG_RECORDS),
		("extents", SpecialFile::Extents, MIN_EXTENT_RECORDS),
		("attributes", SpecialFile::Attributes, MIN_ATTRIBUTE_RECORDS),
	] {
		let fork = volume
			.open_resolved_special_fork(file, OverflowLimits::default())
			.unwrap();
		let mut cases = Cases::default();
		let expected = check_leaves(&fork, |bytes| match file {
			SpecialFile::Catalog => check_node(
				&mut cases,
				bytes,
				|kind, record| catalog_key(kind, record, geometry),
				compare_catalog,
			),
			SpecialFile::Extents => check_node(&mut cases, bytes, extent_key, Ord::cmp),
			SpecialFile::Attributes => check_node(
				&mut cases,
				bytes,
				|kind, record| attribute_key(kind, record, geometry),
				Ord::cmp,
			),
			_ => unreachable!("the witness only uses B-tree special forks"),
		});
		assert_eq!(cases.records, expected, "{label}");
		assert!(cases.records >= minimum_records, "{label}");
		println!(
			"Native packed {label} leaf nodes {} records {} ordered pairs {}",
			cases.nodes, cases.records, cases.pairs
		);
	}
}

fn check_leaves(source: &impl ReadAt<Error: Debug>, mut check: impl FnMut(&[u8])) -> usize {
	let mut prefix = [0; 512];
	source.read_exact_at(ByteOffset(0), &mut prefix).unwrap();
	let width = usize::from(TreeHeader::node_size_hint(&prefix).unwrap());
	let mut bytes = vec![0; width];
	source.read_exact_at(ByteOffset(0), &mut bytes).unwrap();
	let header = TreeHeader::decode(&Node::decode(&bytes).unwrap(), source.len().0).unwrap();
	let mut previous = None;
	let mut next = Some(header.first_leaf.expect("missing native first leaf"));
	let mut visited = 0;
	while let Some(id) = next {
		visited += 1;
		assert!(visited <= MAX_LEAVES && id.0 < header.total_nodes);
		let offset = u64::from(id.0) * u64::from(header.node_bytes);
		source
			.read_exact_at(ByteOffset(offset), &mut bytes)
			.unwrap();
		let node = Node::decode(&bytes).unwrap();
		assert_eq!(node.kind(), Kind::Leaf);
		assert_eq!(node.backward(), previous);
		previous = Some(id);
		next = node.forward();
		check(&bytes);
	}
	assert_eq!(previous, header.last_leaf);
	usize::try_from(header.leaf_records).unwrap()
}

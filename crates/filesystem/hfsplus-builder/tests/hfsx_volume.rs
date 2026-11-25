//! Case-sensitive HFSX construction for H-FMT-03 and H-BUILD-02.

use hfsplus_builder::{
	SealedPlan,
	volume::{
		EntryKind, EntryMetadata, ForkSource, HardLinkGroup, ManifestEntry, VolumeError,
		VolumeFormat, VolumeOptions, plan, plan_with_hard_links,
	},
};
use hfsplus_format::{
	btree::{BINARY_COMPARE, Node, TreeHeader},
	catalog::{FLAG_HAS_FOLDER_COUNT, Record, Value},
	hard_links::is_private_directory_key,
	volume::{CatalogId, Geometry, HEADER_BYTES, Header, PRIMARY_HEADER_OFFSET, Signature},
};

const LABEL: &str = "HFSX Builder Fixture";
const ROOT_ID: CatalogId = CatalogId(2);
const BLOCK_BYTES: u32 = 4096;
const BLOCKS: u32 = 2048;
const IMAGE_BYTES: u64 = BLOCK_BYTES as u64 * BLOCKS as u64;
const UPPER_DIR: &str = "Docs";
const LOWER_DIR: &str = "docs";
const NESTED_DIR: &str = "Docs/Nested";
const UPPER_CASE: &str = "Docs/Case.txt";
const LOWER_CASE: &str = "Docs/case.txt";
const UPPER_PAYLOAD: &[u8] = b"capital spelling\n";
const LOWER_PAYLOAD: &[u8] = b"lowercase spelling\n";
const IGNORABLE_PLAIN: &str = "Docs/join";
const IGNORABLE_SIGNIFICANT: &str = "Docs/jo\u{200d}in";
const FIRST_ALIAS: &str = "Alias";
const SECOND_ALIAS: &str = "alias";

fn options() -> VolumeOptions<'static> {
	let mut options = VolumeOptions::new(
		LABEL,
		Geometry::new(BLOCK_BYTES, BLOCKS, IMAGE_BYTES).unwrap(),
	);
	options.format = VolumeFormat::HfsX;
	options
}

fn folder(path: &str) -> ManifestEntry<'_, &'static [u8]> {
	ManifestEntry {
		path,
		metadata: EntryMetadata::new(0o755),
		kind: EntryKind::Folder,
	}
}

fn file<'a>(path: &'a str, bytes: &'static [u8]) -> ManifestEntry<'a, &'static [u8]> {
	ManifestEntry {
		path,
		metadata: EntryMetadata::new(0o644),
		kind: EntryKind::File {
			data: ForkSource::whole(bytes),
			resource: None,
		},
	}
}

fn sample(reversed: bool) -> SealedPlan<&'static [u8]> {
	let mut entries = vec![
		folder(UPPER_DIR),
		folder(LOWER_DIR),
		folder(NESTED_DIR),
		file(UPPER_CASE, UPPER_PAYLOAD),
		file(LOWER_CASE, LOWER_PAYLOAD),
		file(IGNORABLE_PLAIN, b"plain"),
		file(IGNORABLE_SIGNIFICANT, b"significant"),
	];
	let mut aliases = vec![FIRST_ALIAS, SECOND_ALIAS];
	if reversed {
		entries.reverse();
		aliases.reverse();
	}
	let draft = plan_with_hard_links(
		options(),
		entries,
		vec![HardLinkGroup {
			paths: aliases,
			metadata: EntryMetadata::new(0o644),
			data: ForkSource::whole(b"linked".as_slice()),
			resource: None,
		}],
	)
	.unwrap();
	for (left, right) in [
		(UPPER_DIR, LOWER_DIR),
		(UPPER_CASE, LOWER_CASE),
		(IGNORABLE_PLAIN, IGNORABLE_SIGNIFICANT),
		(FIRST_ALIAS, SECOND_ALIAS),
	] {
		assert_ne!(
			draft.catalog_id(left).unwrap(),
			draft.catalog_id(right).unwrap()
		);
	}
	assert_eq!(draft.catalog_id("DOCS"), None);
	assert_eq!(draft.inode_id(FIRST_ALIAS), draft.inode_id(SECOND_ALIAS));
	draft.seal().unwrap()
}

#[test]
fn keeps_binary_catalog_order_and_records_direct_folder_counts() {
	let mut image = Vec::new();
	sample(false).write_to(&mut image).unwrap();
	let mut reordered = Vec::new();
	sample(true).write_to(&mut reordered).unwrap();
	assert_eq!(image, reordered);

	let offset = PRIMARY_HEADER_OFFSET as usize;
	let header = Header::decode(&image[offset..][..HEADER_BYTES], IMAGE_BYTES).unwrap();
	assert_eq!(header.signature, Signature::HfsX);
	let alternate = header.geometry.alternate_header_offset() as usize;
	assert_eq!(
		image[offset..][..HEADER_BYTES],
		image[alternate..][..HEADER_BYTES]
	);
	let fork = header.forks.catalog;
	let extent = fork.extents().next().unwrap();
	let start = header
		.geometry
		.block_range(extent.start, extent.blocks)
		.unwrap()
		.start as usize;
	let bytes = &image[start..][..fork.logical_bytes as usize];
	let width = usize::from(TreeHeader::node_size_hint(bytes).unwrap());
	let tree =
		TreeHeader::decode(&Node::decode(&bytes[..width]).unwrap(), fork.logical_bytes).unwrap();
	assert_eq!(tree.key_compare_type, BINARY_COMPARE);
	assert_eq!(tree.first_leaf, tree.last_leaf);
	let leaf_start = tree.first_leaf.unwrap().0 as usize * width;
	let leaf = Node::decode(&bytes[leaf_start..][..width]).unwrap();
	let records: Vec<_> = leaf
		.records()
		.map(|raw| Record::decode(raw, header.geometry).unwrap())
		.collect();
	assert!(records.windows(2).all(|pair| {
		let left = pair[0].key;
		let right = pair[1].key;
		left.parent_or_thread
			.cmp(&right.parent_or_thread)
			.then_with(|| left.name.units().cmp(right.name.units()))
			.is_lt()
	}));
	let first_child = records
		.iter()
		.find(|record| record.key.parent_or_thread == ROOT_ID && !record.key.name.is_empty())
		.unwrap();
	assert!(is_private_directory_key(first_child.key));
	for record in records {
		if let Value::Folder {
			metadata,
			tail_word,
			..
		} = record.value
		{
			let count = if metadata.id == ROOT_ID {
				3 // Two visible directories and the hard-link backing directory.
			} else {
				u32::from(record.key.name.units().eq(UPPER_DIR.encode_utf16()))
			};
			assert_ne!(metadata.flags & FLAG_HAS_FOLDER_COUNT, 0);
			assert_eq!(tail_word, count);
		}
	}
}

#[test]
fn still_normalizes_unicode_and_requires_exact_parent_case() {
	assert!(matches!(
		plan(options(), vec![folder("É"), folder("E\u{301}")]),
		Err(VolumeError::PathCollision { .. })
	));
	assert!(matches!(
		plan(
			options(),
			vec![folder(UPPER_DIR), file("docs/Case.txt", UPPER_PAYLOAD)]
		),
		Err(VolumeError::InvalidParent)
	));
}

#[test]
#[ignore = "writes a disposable raw HFSX image for independent readers"]
fn emit_hfsx_volume() {
	let path = std::env::var_os("HFSX_TEST_IMAGE").expect("set a fresh scratch output path");
	let mut file = std::fs::File::create_new(path).unwrap();
	sample(false).write_to(&mut file).unwrap();
}

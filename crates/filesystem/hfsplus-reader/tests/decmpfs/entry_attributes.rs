//! H-READ-01-T01 coverage for stored attributes on both sides of a hard link.

use hfsplus_core::{ByteOffset, ReadAt};
use hfsplus_format::{
	attributes::{InlineRecord, Key},
	btree::Node,
	volume::CatalogId,
};
use hfsplus_reader::{
	Volume,
	attributes::{AttributeReadError, AttributeReadLimits},
	catalog::{
		AttributePresence, AttributeScope, CatalogAttributeError, CatalogError, CatalogLimits,
		HardLinkLimits, Limit, ROOT_ID,
	},
};

use super::{ATTR_LEAF, COMPRESSED, Fixture, INLINE_ID, SHARED};

const VISIBLE_PATH: &str = "inline-hardlink";
const SHARED_ALIAS_PATH: &str = "inline";
const INODE_ATTRIBUTE: &str = "com.apple.attrone";
const VISIBLE_ATTRIBUTE: &str = "com.apple.attrtwo";
const DUPLICATE_ATTRIBUTE: &str = "com.apple.provenance";
const ANCESTOR_ATTRIBUTE: &str = "com.apple.rootdat";
const OTHER_INODE: CatalogId = CatalogId(19);

#[test]
fn lists_both_owners_and_rejects_ambiguous_reads_with_combined_limits() {
	let mut source = Fixture::new(COMPRESSED);
	source.overlay(SHARED);
	let catalog_limits = CatalogLimits::default();
	let hard_links = HardLinkLimits::default();
	let visible = {
		let volume = Volume::open(&source).unwrap();
		let catalog = volume.open_catalog(catalog_limits).unwrap();
		let visible = catalog.lookup(ROOT_ID, VISIBLE_PATH).unwrap().unwrap();
		let file = catalog
			.regular_file(visible.metadata.id, hard_links)
			.unwrap()
			.unwrap();
		assert_eq!(file.inode_id(), INLINE_ID);
		file.id()
	};
	assert!(visible > OTHER_INODE);

	// Reassign the second owner's adjacent native records to the visible
	// link. Preserve record sizes, binary name order and the value bytes.
	let leaf = source.slice_mut(ATTR_LEAF);
	let leaf_start = leaf.as_ptr() as usize;
	let records: Vec<_> = Node::decode(leaf)
		.unwrap()
		.records()
		.map(|raw| raw.as_ptr() as usize - leaf_start)
		.collect();
	let inode_name = utf16(INODE_ATTRIBUTE);
	let visible_name = utf16(VISIBLE_ATTRIBUTE);
	let duplicate_name = utf16(DUPLICATE_ATTRIBUTE);
	for (offset, previous_owner, owner, name) in [
		(records[0], INLINE_ID, INLINE_ID, &inode_name),
		(records[2], OTHER_INODE, visible, &visible_name),
		(records[3], OTHER_INODE, visible, &duplicate_name),
	] {
		let raw = &mut leaf[offset..];
		let (key, remaining) = Key::split(raw).unwrap();
		assert_eq!(key.file_id, previous_owner);
		let width = raw.len() - remaining.len();
		let start_block = key.start_block;
		assert_eq!(Key::encode(owner, name, start_block, raw).unwrap(), width);
	}
	let inode_value = InlineRecord::decode(&leaf[records[0]..records[1]])
		.unwrap()
		.value
		.to_vec();
	let visible_value = InlineRecord::decode(&leaf[records[2]..records[3]])
		.unwrap()
		.value
		.to_vec();

	let volume = Volume::open(source).unwrap();
	let catalog = volume.open_catalog(catalog_limits).unwrap();
	let attribute_limits = AttributeReadLimits::default();
	assert_eq!(
		catalog
			.list_entry_attribute_names(visible, hard_links, attribute_limits)
			.unwrap(),
		[
			inode_name.clone(),
			visible_name.clone(),
			duplicate_name.clone()
		]
	);
	for (name, expected) in [(&inode_name, inode_value), (&visible_name, visible_value)] {
		let value = catalog
			.open_entry_attribute(visible, name, hard_links, attribute_limits)
			.unwrap()
			.unwrap();
		let mut actual = vec![0; expected.len()];
		value.read_exact_at(ByteOffset(0), &mut actual).unwrap();
		assert_eq!(actual, expected);
	}
	assert!(matches!(
		catalog.open_entry_attribute(visible, &duplicate_name, hard_links, attribute_limits),
		Err(CatalogAttributeError::AmbiguousOwner { visible: owner, inode: INLINE_ID })
			if owner == visible
	));
	for name in [&inode_name, &visible_name, &duplicate_name] {
		assert_eq!(
			catalog
				.observe_entry_attribute(
					visible,
					name,
					AttributeScope::Entry,
					hard_links,
					attribute_limits,
				)
				.unwrap(),
			AttributePresence::Present,
		);
	}
	let alias = catalog.lookup(ROOT_ID, SHARED_ALIAS_PATH).unwrap().unwrap();
	assert_eq!(
		catalog
			.list_entry_attribute_names(alias.metadata.id, hard_links, attribute_limits)
			.unwrap(),
		[inode_name, duplicate_name]
	);
	assert!(
		catalog
			.open_entry_attribute(
				alias.metadata.id,
				&visible_name,
				hard_links,
				attribute_limits
			)
			.unwrap()
			.is_none()
	);
	assert!(matches!(
		catalog.list_entry_attribute_names(
			visible,
			hard_links,
			AttributeReadLimits {
				max_entries: 2,
				..attribute_limits
			},
		),
		Err(CatalogAttributeError::Attributes(
			AttributeReadError::EntryLimit
		))
	));
	assert!(matches!(
		catalog.list_entry_attribute_names(INLINE_ID, hard_links, attribute_limits),
		Err(CatalogAttributeError::MissingOwner(INLINE_ID))
	));
}

#[test]
fn observes_selected_ancestry_and_retains_missing_and_budget_evidence() {
	let mut source = Fixture::new(COMPRESSED);
	source.overlay(SHARED);
	let name = utf16(ANCESTOR_ATTRIBUTE);
	// Move the first value to the root with a non-compression name of the
	// same width. Owner sorting and record boundaries remain valid.
	let leaf = source.slice_mut(ATTR_LEAF);
	let raw = Node::decode(leaf).unwrap().records().next().unwrap();
	let at = raw.as_ptr() as usize - leaf.as_ptr() as usize;
	let (key, remaining) = Key::split(raw).unwrap();
	assert_eq!(key.file_id, INLINE_ID);
	let width = raw.len() - remaining.len();
	let start_block = key.start_block;
	assert_eq!(
		Key::encode(ROOT_ID, &name, start_block, &mut leaf[at..]).unwrap(),
		width
	);

	let volume = Volume::open(source).unwrap();
	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	let selected = catalog.lookup(ROOT_ID, VISIBLE_PATH).unwrap().unwrap();
	let id = selected.metadata.id;
	let hard_links = HardLinkLimits::default();
	let attrs = AttributeReadLimits::default();
	for (owner, scope, expected) in [
		(id, AttributeScope::Entry, AttributePresence::Absent),
		(
			id,
			AttributeScope::EntryAndAncestors,
			AttributePresence::Present,
		),
		(
			ROOT_ID,
			AttributeScope::EntryAndAncestors,
			AttributePresence::Present,
		),
		(
			INLINE_ID,
			AttributeScope::EntryAndAncestors,
			AttributePresence::Incomplete,
		),
	] {
		assert_eq!(
			catalog
				.observe_entry_attribute(owner, &name, scope, hard_links, attrs)
				.unwrap(),
			expected,
		);
	}
	assert!(matches!(
		catalog.observe_entry_attribute(
			ROOT_ID,
			&name,
			AttributeScope::Entry,
			hard_links,
			AttributeReadLimits {
				max_nodes: 0,
				..attrs
			},
		),
		Err(CatalogAttributeError::Attributes(
			AttributeReadError::NodeLimit
		))
	));
	let limited = volume
		.open_catalog(CatalogLimits {
			max_ancestors: 0,
			..CatalogLimits::default()
		})
		.unwrap();
	assert!(matches!(
		limited.observe_entry_attribute(
			id,
			&name,
			AttributeScope::EntryAndAncestors,
			hard_links,
			attrs
		),
		Err(CatalogAttributeError::Catalog(CatalogError::Limit(
			Limit::Ancestors
		)))
	));
}

fn utf16(name: &str) -> Vec<u16> {
	name.encode_utf16().collect()
}

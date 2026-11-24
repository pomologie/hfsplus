//! Inline attribute storage and allocation checks for H-BUILD-03-T03.

use hfsplus_builder::{
	tree::TreeError,
	volume::{
		EntryKind, EntryMetadata, ForkSource, ManifestEntry, VolumeDraft, VolumeError,
		VolumeOptions, plan, validate_inline_attribute,
	},
};
use hfsplus_core::{ByteLen, ByteOffset, ReadAt};
use hfsplus_format::{
	Error,
	attributes::{
		InlineRecord, MAX_KEY_LENGTH, NATIVE_INLINE_MAX_VALUE_BYTES, NATIVE_KEY_COMPARE_TYPE,
		NATIVE_NODE_BYTES,
	},
	btree::{BIG_KEYS_MASK, Kind, Node, TreeHeader, VARIABLE_INDEX_KEYS_MASK},
	catalog::{FLAG_HAS_ATTRIBUTES, Record as CatalogRecord, Value as CatalogValue},
	volume::{CatalogId, Fork, Geometry, HEADER_BYTES, Header, PRIMARY_HEADER_OFFSET},
};

const LABEL: &str = "Inline Attributes";
const BLOCK_BYTES: u32 = 4096;
const BLOCKS: u32 = 2048;
const IMAGE_BYTES: u64 = BLOCK_BYTES as u64 * BLOCKS as u64;
const DIRECTORY: &str = "Café";
const FILE: &str = "Café/Document";
const FILE_ALIAS: &str = "CAFE\u{301}/DOCUMENT";
const UNATTRIBUTED: &str = "Unattributed";
const ATTRIBUTE: &str = "org.pomologie.value";
const QUARANTINE: &str = "com.apple.quarantine";
const QUARANTINE_VALUE: &[u8] = b"0081;00000000;PomologieFixture;";
const MAX_VALUE: &[u8] = &[0xa5; NATIVE_INLINE_MAX_VALUE_BYTES];
const LARGE_RECORDS: usize = 11;
const FILE_DATA: &[u8] = b"attribute payload";
const RESOURCE_DATA: &[u8] = b"resource bytes";
const BITMAP_FIRST_BIT: u8 = 0x80;

fn geometry() -> Geometry {
	Geometry::new(BLOCK_BYTES, BLOCKS, IMAGE_BYTES).unwrap()
}

fn entries() -> Vec<ManifestEntry<'static, &'static [u8]>> {
	vec![
		ManifestEntry {
			path: FILE,
			metadata: EntryMetadata::new(0o644),
			kind: EntryKind::File {
				data: ForkSource::whole(FILE_DATA),
				resource: Some(ForkSource::whole(RESOURCE_DATA)),
			},
		},
		ManifestEntry {
			path: UNATTRIBUTED,
			metadata: EntryMetadata::new(0o644),
			kind: EntryKind::File {
				data: ForkSource::whole(&[][..]),
				resource: None,
			},
		},
		ManifestEntry {
			path: DIRECTORY,
			metadata: EntryMetadata::new(0o755),
			kind: EntryKind::Folder,
		},
	]
}

fn draft() -> VolumeDraft<'static, &'static [u8]> {
	plan(VolumeOptions::new(LABEL, geometry()), entries()).unwrap()
}

fn fork_bytes<'a>(image: &'a [u8], fork: &Fork) -> &'a [u8] {
	let extent = fork.extents().next().unwrap();
	let extent_bytes = geometry().block_range(extent.start, extent.blocks).unwrap();
	assert_eq!(fork.extents().count(), 1);
	&image[extent_bytes.start as usize..][..fork.logical_bytes as usize]
}

fn leaves(bytes: &[u8]) -> (TreeHeader, Vec<&[u8]>) {
	let width = usize::from(TreeHeader::node_size_hint(bytes).unwrap());
	let tree =
		TreeHeader::decode(&Node::decode(&bytes[..width]).unwrap(), bytes.len() as u64).unwrap();
	let mut records = Vec::new();
	let mut previous = None;
	let mut leaf = tree.first_leaf;
	let mut visited = 0;
	while let Some(id) = leaf {
		visited += 1;
		assert!(visited <= tree.total_nodes);
		let start = id.0 as usize * width;
		let node = Node::decode(&bytes[start..][..width]).unwrap();
		assert_eq!(node.kind(), Kind::Leaf);
		assert_eq!(node.backward(), previous);
		records.extend(node.records());
		previous = leaf;
		leaf = node.forward();
	}
	assert_eq!(tree.last_leaf, previous);
	assert_eq!(tree.leaf_records as usize, records.len());
	(tree, records)
}

#[test]
fn packs_exact_inline_values_by_catalog_owner_and_raw_utf16_name() {
	let mut attributes = vec![
		("", ATTRIBUTE.to_owned(), &[][..]),
		(DIRECTORY, ATTRIBUTE.to_owned(), b"folder".as_slice()),
		(FILE_ALIAS, QUARANTINE.to_owned(), QUARANTINE_VALUE),
		(
			FILE,
			"org.pomologie.e\u{301}".to_owned(),
			b"decomposed".as_slice(),
		),
		(FILE, "org.pomologie.é".to_owned(), b"composed".as_slice()),
		(
			FILE,
			"org.pomologie.😀".to_owned(),
			b"supplementary".as_slice(),
		),
		(FILE, "org.pomologie.\u{e000}".to_owned(), b"bmp".as_slice()),
	];
	for number in 0..LARGE_RECORDS {
		attributes.push((FILE, format!("org.pomologie.large{number:02}"), MAX_VALUE));
	}
	let mut draft = draft();
	let root = draft.catalog_id("").unwrap();
	let folder = draft.catalog_id(DIRECTORY).unwrap();
	let file = draft.catalog_id(FILE).unwrap();
	let unattributed = draft.catalog_id(UNATTRIBUTED).unwrap();
	let mut expected: Vec<_> = attributes
		.iter()
		.map(|(path, name, value)| {
			(
				draft.catalog_id(path).unwrap(),
				name.encode_utf16().collect::<Vec<_>>(),
				*value,
			)
		})
		.collect();
	expected.sort_unstable();
	let mut reverse_entries = entries();
	reverse_entries.reverse();
	let mut reordered = plan(VolumeOptions::new(LABEL, geometry()), reverse_entries).unwrap();
	for (path, name, value) in &attributes {
		draft.add_inline_attribute(path, name, value).unwrap();
	}
	for (path, name, value) in attributes.iter().rev() {
		reordered.add_inline_attribute(path, name, value).unwrap();
	}
	let mut image = Vec::new();
	draft.seal().unwrap().write_to(&mut image).unwrap();
	let mut second = Vec::new();
	reordered.seal().unwrap().write_to(&mut second).unwrap();
	assert_eq!(image, second);

	let primary = &image[PRIMARY_HEADER_OFFSET as usize..][..HEADER_BYTES];
	let alternate = &image[geometry().alternate_header_offset() as usize..][..HEADER_BYTES];
	assert_eq!(primary, alternate);
	let header = Header::decode(primary, IMAGE_BYTES).unwrap();
	let (tree, records) = leaves(fork_bytes(&image, &header.forks.attributes));
	assert!(tree.depth > 1);
	assert_eq!(tree.node_bytes, NATIVE_NODE_BYTES);
	assert_eq!(usize::from(tree.max_key_bytes), MAX_KEY_LENGTH);
	assert_eq!(tree.key_compare_type, NATIVE_KEY_COMPARE_TYPE);
	assert_eq!(tree.attributes, BIG_KEYS_MASK | VARIABLE_INDEX_KEYS_MASK);
	assert_eq!(records.len(), expected.len());
	for (bytes, (owner, name, value)) in records.into_iter().zip(expected) {
		let actual = InlineRecord::decode(bytes).unwrap();
		assert_eq!(actual.key.file_id, owner);
		assert!(actual.key.name.units().eq(name));
		assert_eq!(actual.key.start_block, 0);
		assert_eq!(actual.value, value);
	}

	let attributes = header.forks.attributes.extents().next().unwrap();
	let bitmap = fork_bytes(&image, &header.forks.allocation);
	let allocated = |block: u32| {
		bitmap[(block / u8::BITS) as usize] & (BITMAP_FIRST_BIT >> (block % u8::BITS)) != 0
	};
	assert!((attributes.start.0..attributes.start.0 + attributes.blocks).all(allocated));
	assert_eq!(
		(0..BLOCKS).filter(|&block| !allocated(block)).count(),
		header.free_blocks as usize
	);

	let (_, catalog) = leaves(fork_bytes(&image, &header.forks.catalog));
	let owners: Vec<CatalogId> = catalog
		.into_iter()
		.filter_map(|record| {
			let record = CatalogRecord::decode(record, geometry()).unwrap();
			match record.value {
				CatalogValue::Folder { metadata, .. } | CatalogValue::File { metadata, .. }
					if metadata.flags & FLAG_HAS_ATTRIBUTES != 0 =>
				{
					Some(metadata.id)
				}
				_ => None,
			}
		})
		.collect();
	assert_eq!(owners, [root, folder, file]);
	assert!(!owners.contains(&unattributed));
}

#[test]
fn rejects_unknown_owners_invalid_names_and_unsupported_profiles() {
	let mut draft = draft();
	assert!(matches!(
		draft.add_inline_attribute("missing", ATTRIBUTE, &[]),
		Err(VolumeError::MissingAttributeOwner)
	));
	assert!(matches!(
		draft.add_inline_attribute(FILE, "invalid\0name", &[]),
		Err(VolumeError::InvalidAttributeName)
	));
	for name in [
		"com.apple.FinderInfo",
		"com.apple.ResourceFork",
		"com.apple.system.Security",
	] {
		assert!(matches!(
			validate_inline_attribute(name, 0),
			Err(VolumeError::SpecialAttribute)
		));
		assert!(matches!(
			draft.add_inline_attribute(FILE, name, &[]),
			Err(VolumeError::SpecialAttribute)
		));
	}
	assert!(matches!(
		draft.add_inline_attribute(FILE, "com.apple.decmpfs", &[]),
		Err(VolumeError::Format(Error::UnsupportedCompressedAttribute))
	));
	const TOO_LARGE: usize = NATIVE_INLINE_MAX_VALUE_BYTES + 1;
	assert!(matches!(
		draft.add_inline_attribute(FILE, ATTRIBUTE, &[0; TOO_LARGE]),
		Err(VolumeError::Format(Error::UnsupportedAttributeSize(
			TOO_LARGE
		)))
	));
	assert!(matches!(
		validate_inline_attribute(ATTRIBUTE, TOO_LARGE),
		Err(VolumeError::Format(Error::UnsupportedAttributeSize(
			TOO_LARGE
		)))
	));
	let id = draft.catalog_id(FILE).unwrap();
	draft.add_inline_attribute(FILE, ATTRIBUTE, b"one").unwrap();
	draft
		.add_inline_attribute(FILE_ALIAS, ATTRIBUTE, b"two")
		.unwrap();
	assert!(matches!(draft.seal(), Err(VolumeError::DuplicateAttribute(owner)) if owner == id));
}

#[test]
fn charges_the_attributes_tree_against_the_same_plan_metadata_budget() {
	const BUDGET: ByteLen = ByteLen(16 * 1024);
	let opts = || {
		let mut options = VolumeOptions::new(LABEL, geometry());
		options.limits.max_metadata_bytes = BUDGET;
		options
	};
	let baseline = plan(opts(), entries()).unwrap().seal().unwrap();
	let mut header_bytes = [0; HEADER_BYTES];
	baseline
		.read_exact_at(ByteOffset(PRIMARY_HEADER_OFFSET), &mut header_bytes)
		.unwrap();
	let header = Header::decode(&header_bytes, IMAGE_BYTES).unwrap();
	assert_eq!(header.forks.attributes, Fork::empty());
	let mut draft = plan(opts(), entries()).unwrap();
	draft.add_inline_attribute(FILE, ATTRIBUTE, &[]).unwrap();
	assert!(matches!(
		draft.seal(),
		Err(VolumeError::Tree(TreeError::NodeLimit))
	));
}

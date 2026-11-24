//! Shared fork, catalog identity and capacity checks for H-BUILD-04.

use hfsplus_builder::{
	RangeKind,
	volume::{
		CapacityError, CapacityPolicy, EntryKind, EntryMetadata, ForkSource, HardLinkGroup,
		ManifestEntry, VolumeDraft, VolumeError, VolumeOptions, plan_with_hard_links,
	},
};
use hfsplus_core::{ByteLen, names::NameCollation};
use hfsplus_format::{
	attributes::InlineRecord,
	btree::{Kind, Node, TreeHeader},
	catalog::{FLAG_HAS_ATTRIBUTES, Record, ThreadKind, Value},
	hard_links::{Inode, Stub, is_private_directory_key},
	volume::{CatalogId, Fork, Geometry, HEADER_BYTES, Header, PRIMARY_HEADER_OFFSET},
};

const LABEL: &str = "Builder Hard Links";
const BLOCK_BYTES: u32 = 4096;
const IMAGE_BYTES: u64 = 8 << 20;
const FOLDER: &str = "nested";
const FIRST: &str = "one";
const SECOND: &str = "two";
const THIRD: &str = "nested/three";
const FOURTH: &str = "four";
const FIFTH: &str = "nested/five";
const COPIED: &str = "independent copy";
const FIRST_DATA: &[u8] = b"pomologie linked file\n";
const SECOND_DATA: &[u8] = b"pomologie second group\n";
const RESOURCE: &[u8] = b"pomologie resource\0fork\n";
const ATTRIBUTE: &str = "org.pomologie.hardlink.sample";
const ATTRIBUTE_VALUE: &[u8] = &[0, 0x11, 0x22, 0xff];
const CREATED: u32 = 3_787_040_096;
const ROOT_ID: CatalogId = CatalogId(2);
const SECTOR_BYTES: u64 = 512;

fn options() -> VolumeOptions<'static> {
	VolumeOptions::new(
		LABEL,
		Geometry::new(
			BLOCK_BYTES,
			(IMAGE_BYTES / u64::from(BLOCK_BYTES)) as u32,
			IMAGE_BYTES,
		)
		.unwrap(),
	)
}

fn group<'a>(paths: Vec<&'a str>) -> HardLinkGroup<'a, &'static [u8]> {
	HardLinkGroup {
		paths,
		metadata: EntryMetadata {
			uid: 501,
			gid: 20,
			dates: [CREATED; 5],
			..EntryMetadata::new(0o751)
		},
		data: ForkSource::whole(FIRST_DATA),
		resource: Some(ForkSource::whole(RESOURCE)),
	}
}

fn groups() -> Vec<HardLinkGroup<'static, &'static [u8]>> {
	vec![
		group(vec![FIRST, SECOND, THIRD]),
		HardLinkGroup {
			paths: vec![FOURTH, FIFTH],
			metadata: EntryMetadata::new(0o640),
			data: ForkSource::whole(SECOND_DATA),
			resource: None,
		},
	]
}

fn entries() -> Vec<ManifestEntry<'static, &'static [u8]>> {
	vec![
		ManifestEntry {
			path: FOLDER,
			metadata: EntryMetadata::new(0o755),
			kind: EntryKind::Folder,
		},
		ManifestEntry {
			path: COPIED,
			metadata: EntryMetadata::new(0o644),
			kind: EntryKind::File {
				data: ForkSource::whole(FIRST_DATA),
				resource: None,
			},
		},
	]
}

fn draft(reversed: bool) -> VolumeDraft<'static, &'static [u8]> {
	let mut entries = entries();
	let mut groups = groups();
	if reversed {
		entries.reverse();
		groups.reverse();
		for group in &mut groups {
			group.paths.reverse();
		}
	}
	let mut draft = plan_with_hard_links(options(), entries, groups).unwrap();
	draft
		.add_inline_attribute(THIRD, ATTRIBUTE, ATTRIBUTE_VALUE)
		.unwrap();
	draft
}

fn header(image: &[u8]) -> Header {
	let start = PRIMARY_HEADER_OFFSET as usize;
	Header::decode(&image[start..][..HEADER_BYTES], image.len() as u64).unwrap()
}

fn fork_bytes<'a>(image: &'a [u8], geometry: Geometry, fork: &Fork) -> &'a [u8] {
	let extent = fork.extents().next().unwrap();
	assert_eq!(fork.extents().count(), 1);
	let range = geometry.block_range(extent.start, extent.blocks).unwrap();
	&image[range.start as usize..][..fork.logical_bytes as usize]
}

fn leaf_records(bytes: &[u8]) -> Vec<&[u8]> {
	let width = usize::from(TreeHeader::node_size_hint(bytes).unwrap());
	let tree =
		TreeHeader::decode(&Node::decode(&bytes[..width]).unwrap(), bytes.len() as u64).unwrap();
	let mut records = Vec::new();
	let mut leaf = tree.first_leaf;
	let mut last = None;
	for _ in 0..tree.total_nodes {
		let Some(id) = leaf else { break };
		let start = id.0 as usize * width;
		let node = Node::decode(&bytes[start..][..width]).unwrap();
		assert_eq!(node.kind(), Kind::Leaf);
		assert_eq!(node.backward(), last);
		records.extend(node.records());
		last = leaf;
		leaf = node.forward();
	}
	assert_eq!(leaf, None);
	assert_eq!(last, tree.last_leaf);
	assert_eq!(records.len(), tree.leaf_records as usize);
	records
}

#[test]
fn writes_two_distinct_groups_and_one_independent_copy() {
	let draft = draft(false);
	let paths = [FIRST, SECOND, THIRD, FOURTH, FIFTH, COPIED];
	let ids: Vec<_> = paths
		.iter()
		.map(|path| draft.catalog_id(path).unwrap())
		.collect();
	let inodes: Vec<_> = paths
		.iter()
		.map(|path| draft.inode_id(path).unwrap())
		.collect();
	let visible_ids: std::collections::BTreeSet<_> = ids.iter().copied().collect();
	assert_eq!(visible_ids.len(), paths.len());
	assert_eq!(inodes[..3], [inodes[0]; 3]);
	assert_eq!(inodes[3], inodes[4]);
	assert_ne!(inodes[0], inodes[3]);
	assert_eq!(inodes[5], ids[5]);
	assert!(!visible_ids.contains(&inodes[0]));
	assert!(!visible_ids.contains(&inodes[3]));
	assert_eq!(draft.inode_id("NESTED/THREE"), Some(inodes[0]));
	assert_eq!(draft.inode_id(""), Some(ROOT_ID));

	let sealed = draft.seal().unwrap();
	let source_ranges = sealed
		.ranges()
		.filter(|(_, kind)| *kind == RangeKind::Source)
		.count();
	assert_eq!(source_ranges, 4);
	let mut image = Vec::new();
	sealed.write_to(&mut image).unwrap();
	let header = header(&image);
	assert_eq!(header.files, 8);
	assert_eq!(header.folders, 2);
	assert_eq!(header.next_catalog_id.0, inodes[0].0.max(inodes[3].0) + 1);
	let bytes = fork_bytes(&image, header.geometry, &header.forks.catalog);
	let leaves = leaf_records(bytes);
	let records: Vec<_> = leaves
		.iter()
		.map(|raw| Record::decode(raw, header.geometry).unwrap())
		.collect();
	let private = records
		.iter()
		.find(|record| is_private_directory_key(record.key))
		.unwrap();
	let Value::Folder {
		metadata,
		valence: 2,
		..
	} = private.value
	else {
		panic!("expected the private inode directory");
	};
	let private_id = metadata.id;
	assert!(matches!(
		records
			.iter()
			.find(
				|record| matches!(record.value, Value::Folder { metadata, .. } if metadata.id == ROOT_ID)
			)
			.unwrap()
			.value,
		Value::Folder { valence: 6, .. }
	));
	assert!(records.iter().any(|record| matches!(
		record.value,
		Value::Thread {
			kind: ThreadKind::Folder,
			parent: ROOT_ID,
			..
		}
	) && record.key.parent_or_thread == private_id));
	let first_ref = records
		.iter()
		.find_map(|record| {
			let Value::File { metadata, .. } = record.value else {
				return None;
			};
			(metadata.id == ids[0]).then(|| Stub::decode(record).unwrap().unwrap().reference)
		})
		.unwrap();
	let mut physical = Vec::new();
	for (record, raw) in records.iter().zip(&leaves) {
		if let Some(inode) = Inode::decode(raw, private_id, header.geometry).unwrap() {
			let first = inode.metadata.id == inodes[0];
			assert_eq!(inode.link_count.get(), if first { 3 } else { 2 });
			assert_eq!(inode.first_link, None);
			assert_eq!(
				inode.metadata.flags & FLAG_HAS_ATTRIBUTES,
				if first { FLAG_HAS_ATTRIBUTES } else { 0 }
			);
			assert_eq!(
				fork_bytes(&image, header.geometry, &inode.data),
				if first { FIRST_DATA } else { SECOND_DATA }
			);
			if first {
				assert_eq!(inode.reference, first_ref);
				assert_eq!(inode.metadata.permissions.mode, 0o100751);
				assert_eq!(inode.metadata.permissions.uid, 501);
				assert_eq!(inode.metadata.permissions.gid, 20);
				assert_eq!(inode.metadata.dates, [CREATED; 5]);
				assert_eq!(
					fork_bytes(&image, header.geometry, &inode.resource),
					RESOURCE
				);
			} else {
				assert_ne!(inode.reference, first_ref);
				assert_eq!(inode.resource, Fork::empty());
			}
			physical.push(inode.metadata.id);
		}
		if let Some(stub) = Stub::decode(record).unwrap() {
			assert!(matches!(record.value, Value::File {
				metadata, data, resource
			} if ids[..5].contains(&metadata.id) && data == Fork::empty() && resource == Fork::empty()));
			assert_eq!(stub.neighbors, None);
		}
	}
	assert_eq!(physical.len(), 2);
	let attributes = leaf_records(fork_bytes(
		&image,
		header.geometry,
		&header.forks.attributes,
	));
	assert_eq!(attributes.len(), 1);
	let attribute = InlineRecord::decode(attributes[0]).unwrap();
	assert_eq!(attribute.key.file_id, inodes[0]);
	assert_eq!(attribute.value, ATTRIBUTE_VALUE);
}

#[test]
fn reordered_groups_and_aliases_have_identical_capacity_identities_and_bytes() {
	let mut forward = draft(false);
	let mut reverse = draft(true);
	for path in [FIRST, SECOND, THIRD, FOURTH, FIFTH, COPIED] {
		assert_eq!(forward.catalog_id(path), reverse.catalog_id(path));
		assert_eq!(forward.inode_id(path), reverse.inode_id(path));
	}
	let automatic = CapacityPolicy::Auto {
		free_space: ByteLen(1),
	};
	let capacity = forward.capacity(automatic).unwrap();
	assert_eq!(capacity, reverse.capacity(automatic).unwrap());
	let short = CapacityPolicy::Exact {
		bytes: ByteLen(capacity.image_bytes().0 - SECTOR_BYTES),
		free_space: ByteLen(1),
	};
	assert!(matches!(
		forward.capacity(short),
		Err(CapacityError::InsufficientSpace { .. })
	));
	let mut first = Vec::new();
	let mut second = Vec::new();
	forward
		.seal_with_capacity(automatic)
		.unwrap()
		.write_to(&mut first)
		.unwrap();
	reverse
		.seal_with_capacity(automatic)
		.unwrap()
		.write_to(&mut second)
		.unwrap();
	assert_eq!(first.len() as u64, capacity.image_bytes().0);
	assert_eq!(first, second);
}

#[test]
fn rejects_invalid_group_members_and_duplicate_shared_attributes() {
	assert!(matches!(
		plan_with_hard_links(options(), entries(), vec![group(vec![FIRST])]),
		Err(VolumeError::InvalidHardLinkGroup)
	));
	assert!(matches!(
		plan_with_hard_links(options(), entries(), vec![group(vec![FIRST, COPIED])]),
		Err(VolumeError::PathCollision { .. })
	));
	assert!(matches!(
		plan_with_hard_links(options(), entries(), vec![group(vec![FIRST, "ONE"])]),
		Err(VolumeError::PathCollision { .. })
	));
	assert!(matches!(
		plan_with_hard_links(options(), vec![], vec![group(vec![FIRST, "one/child"])]),
		Err(VolumeError::InvalidParent)
	));
	let mut constrained = options();
	constrained.max_entries = 6;
	assert!(matches!(
		plan_with_hard_links(constrained, entries(), groups()),
		Err(VolumeError::Limit)
	));
	let mut draft = draft(false);
	assert!(matches!(
		draft.bind_generated_data(FIRST, ForkSource::whole(SECOND_DATA)),
		Err(VolumeError::NotAwaitingGeneratedData)
	));
	let owner = draft.inode_id(SECOND).unwrap();
	draft
		.add_inline_attribute(SECOND, ATTRIBUTE, ATTRIBUTE_VALUE)
		.unwrap();
	assert!(matches!(draft.seal(), Err(VolumeError::DuplicateAttribute(id)) if id == owner));
}

#[test]
fn sorts_private_catalog_keys_across_decimal_reference_widths() {
	const GROUPS: usize = 901;
	let names: Vec<_> = (0..GROUPS)
		.map(|rank| (format!("A{rank:04}"), format!("B{rank:04}")))
		.collect();
	let groups = names
		.iter()
		.map(|(a, b)| HardLinkGroup {
			paths: vec![a, b],
			metadata: EntryMetadata::new(0o644),
			data: ForkSource::whole(&[][..]),
			resource: None,
		})
		.collect();
	let draft = plan_with_hard_links(options(), vec![], groups).unwrap();
	let id_999 = draft.inode_id("A0899").unwrap();
	let id_1000 = draft.inode_id("A0900").unwrap();
	assert!(id_1000 < id_999);
	let mut image = Vec::new();
	draft.seal().unwrap().write_to(&mut image).unwrap();
	let header = header(&image);
	let catalog = leaf_records(fork_bytes(&image, header.geometry, &header.forks.catalog));
	let directory = catalog
		.iter()
		.find_map(|raw| {
			let record = Record::decode(raw, header.geometry).unwrap();
			match record.value {
				Value::Folder { metadata, .. } if is_private_directory_key(record.key) => {
					Some(metadata.id)
				}
				_ => None,
			}
		})
		.unwrap();
	let private_names: Vec<Vec<u16>> = catalog
		.into_iter()
		.filter_map(|raw| {
			let record = Record::decode(raw, header.geometry).unwrap();
			(record.key.parent_or_thread == directory && !record.key.name.is_empty())
				.then(|| record.key.name.units().collect())
		})
		.collect();
	assert_eq!(private_names.len(), GROUPS);
	assert!(private_names.windows(2).all(|pair| {
		NameCollation::CaseFolded
			.compare(&pair[0], &pair[1])
			.is_lt()
	}));
}

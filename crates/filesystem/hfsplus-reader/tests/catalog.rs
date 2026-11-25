//! Native HFS+ catalog coverage for H-READ-01 and seek invariants of H-V08.

use std::num::NonZeroUsize;

use hfsplus_core::{ByteOffset, ReadAt};
use hfsplus_format::{
	btree::{Node, TreeHeader},
	catalog::{Key, Record, Value},
	volume::{CatalogId, HEADER_BYTES, Header, PRIMARY_HEADER_OFFSET},
};
use hfsplus_reader::{
	Volume,
	catalog::{CatalogError, CatalogLimits, Corruption, EntryKind, Limit, ROOT_ID},
};

const NATIVE: &[u8] = include_bytes!("fixtures/native-catalog-volume.bin");
const LABEL: &str = "Pomologie Catalog";
const NESTED: &str = "Nested";
const FOLDED_FILE: &str = "RÉSUMÉ.TXT";
const STORED_FILE: &str = "Re\u{0301}sume\u{0301}.txt";
const PAYLOAD: &[u8] = b"catalog unicode payload\n";
const EMPTY_FILES: usize = 96;
const NATIVE_NODE_BYTES: usize = 4096;
const HEADER_MAP_ORDINAL: usize = 2;
const ROOT_VALENCE_OFFSET: usize = 4;
const INDEX_CHILD_BYTES: usize = size_of::<u32>();
const NODE_BACK_LINK_OFFSET: usize = size_of::<u32>();
const PAGE_SIZE: NonZeroUsize = NonZeroUsize::new(13).unwrap();

#[test]
fn catalog_retains_its_exact_volume_view_identity() {
	let volume = Volume::open(NATIVE).unwrap();
	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	assert!(catalog.belongs_to(&volume));
	assert!(!catalog.belongs_to(&Volume::open(NATIVE).unwrap()));
}

#[test]
fn resolves_native_multilevel_names_ids_and_payload() {
	let volume = Volume::open(NATIVE).unwrap();
	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	let root = catalog.root().unwrap();
	assert_eq!(root.metadata.id, ROOT_ID);
	assert!(root.name.iter().copied().eq(LABEL.encode_utf16()));
	let folder = catalog.lookup(ROOT_ID, "nested").unwrap().unwrap();
	assert!(folder.name.iter().copied().eq(NESTED.encode_utf16()));
	let file = catalog
		.lookup(folder.metadata.id, FOLDED_FILE)
		.unwrap()
		.unwrap();
	assert!(file.name.iter().copied().eq(STORED_FILE.encode_utf16()));
	assert_eq!(catalog.entry(file.metadata.id).unwrap(), Some(file.clone()));
	assert_eq!(
		catalog.lookup_raw(folder.metadata.id, &file.name).unwrap(),
		Some(file.clone())
	);
	assert_eq!(catalog.lookup(folder.metadata.id, "absent").unwrap(), None);
	assert!(
		matches!(catalog.read_dir(file.metadata.id), Err(CatalogError::ExpectedDirectory(id)) if id == file.metadata.id)
	);
	assert!(matches!(
		catalog.lookup(ROOT_ID, "\u{200c}"),
		Err(CatalogError::InvalidName)
	));
	let EntryKind::File { data, resource } = file.kind else {
		panic!("expected a native regular file");
	};
	let fork = volume.open_embedded_fork(&data).unwrap();
	let mut read = [0; PAYLOAD.len()];
	fork.read_exact_at(ByteOffset(0), &mut read).unwrap();
	assert_eq!(&read, PAYLOAD);
	assert!(volume.open_embedded_fork(&resource).unwrap().is_empty());
}

#[test]
fn enumerates_a_native_directory_across_leaf_and_page_boundaries() {
	let volume = Volume::open(NATIVE).unwrap();
	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	let mut directory = catalog.read_dir(ROOT_ID).unwrap();
	let mut names = Vec::new();
	loop {
		let page = directory.next_page(PAGE_SIZE).unwrap();
		assert!(page.entries.len() <= PAGE_SIZE.get());
		names.extend(
			page.entries
				.into_iter()
				.map(|item| String::from_utf16(&item.name).unwrap()),
		);
		if page.complete {
			break;
		}
	}
	let mut expected: Vec<_> = (0..EMPTY_FILES).map(|i| format!("item-{i:03}")).collect();
	expected.push(NESTED.into());
	assert_eq!(names, expected);
	let terminal = directory.next_page(PAGE_SIZE).unwrap();
	assert!(terminal.complete && terminal.entries.is_empty());
}

#[test]
fn bounds_node_queries_pages_and_allocation_map() {
	let volume = Volume::open(NATIVE).unwrap();
	let base = CatalogLimits::default();
	for (limits, expected) in [
		(
			CatalogLimits {
				max_depth: 1,
				..base
			},
			Limit::Depth,
		),
		(
			CatalogLimits {
				max_map_bytes: 1,
				..base
			},
			Limit::AllocationMap,
		),
	] {
		assert!(
			matches!(volume.open_catalog(limits), Err(CatalogError::Limit(limit)) if limit == expected)
		);
	}
	let catalog = volume
		.open_catalog(CatalogLimits {
			max_nodes_per_query: 1,
			..base
		})
		.unwrap();
	assert!(matches!(
		catalog.root(),
		Err(CatalogError::Limit(Limit::Nodes))
	));
	let catalog = volume
		.open_catalog(CatalogLimits {
			max_page_entries: 1,
			..base
		})
		.unwrap();
	let mut directory = catalog.read_dir(ROOT_ID).unwrap();
	assert!(matches!(
		directory.next_page(PAGE_SIZE),
		Err(CatalogError::Limit(Limit::PageEntries))
	));
	assert_eq!(
		directory
			.next_page(NonZeroUsize::MIN)
			.unwrap()
			.entries
			.len(),
		1
	);
}

#[test]
fn rejects_unallocated_roots_and_self_pointing_index_records() {
	let layout = layout(NATIVE);
	let root = layout.header.root.unwrap().0 as usize;
	let node = Node::decode(&NATIVE[layout.base..layout.base + NATIVE_NODE_BYTES]).unwrap();
	let map = node.record(HEADER_MAP_ORDINAL).unwrap();
	let map_byte = offset_of(NATIVE, map) + root / u8::BITS as usize;
	let mut image = NATIVE.to_vec();
	image[map_byte] &= !(0x80 >> (root % u8::BITS as usize));
	let volume = Volume::open(image.as_slice()).unwrap();
	assert!(matches!(
		volume.open_catalog(CatalogLimits::default()),
		Err(CatalogError::Corrupt {
			reason: Corruption::AllocationMap,
			..
		})
	));

	let mut image = NATIVE.to_vec();
	let root_offset = layout.base + root * NATIVE_NODE_BYTES;
	let root_bytes = &NATIVE[root_offset..root_offset + NATIVE_NODE_BYTES];
	let index = Node::decode(root_bytes).unwrap();
	let first = index.record(0).unwrap();
	let pointer = offset_of(NATIVE, first) + first.len() - INDEX_CHILD_BYTES;
	image[pointer..pointer + INDEX_CHILD_BYTES].copy_from_slice(&(root as u32).to_be_bytes());
	let volume = Volume::open(image.as_slice()).unwrap();
	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	assert!(matches!(
		catalog.root(),
		Err(CatalogError::Corrupt {
			reason: Corruption::IndexRoute,
			..
		})
	));
}

#[test]
fn detects_a_wrong_root_valence_and_keeps_a_failed_page_retryable() {
	let layout = layout(NATIVE);
	let leaf = layout.header.first_leaf.unwrap().0 as usize;
	let start = layout.base + NATIVE_NODE_BYTES * leaf;
	let leaf = Node::decode(&NATIVE[start..start + NATIVE_NODE_BYTES]).unwrap();
	let root = leaf
		.records()
		.find(|raw| {
			matches!(
				Record::decode(raw, layout.volume.geometry).unwrap().value,
				Value::Folder { metadata, .. } if metadata.id == ROOT_ID
			)
		})
		.unwrap();
	let (_, data) = Key::split_leaf(root).unwrap();
	let valence = offset_of(NATIVE, data) + ROOT_VALENCE_OFFSET;
	let mut image = NATIVE.to_vec();
	image[valence..valence + size_of::<u32>()]
		.copy_from_slice(&(EMPTY_FILES as u32 + 2).to_be_bytes());
	let volume = Volume::open(image.as_slice()).unwrap();
	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	let mut directory = catalog.read_dir(ROOT_ID).unwrap();
	while let Ok(page) = directory.next_page(PAGE_SIZE) {
		assert!(!page.complete);
	}
	assert!(matches!(
		directory.next_page(PAGE_SIZE),
		Err(CatalogError::Corrupt {
			reason: Corruption::Valence,
			..
		})
	));
	assert_eq!(
		catalog
			.entry(CatalogId::FIRST_USER)
			.unwrap()
			.unwrap()
			.parent,
		ROOT_ID
	);
}

#[test]
fn rejects_a_thread_with_a_case_equivalent_but_different_display_name() {
	let volume = Volume::open(NATIVE).unwrap();
	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	let parent = catalog
		.lookup(ROOT_ID, NESTED)
		.unwrap()
		.unwrap()
		.metadata
		.id;
	let id = catalog
		.lookup(parent, STORED_FILE)
		.unwrap()
		.unwrap()
		.metadata
		.id;
	let layout = layout(NATIVE);
	let mut image = NATIVE.to_vec();
	let file_thread = (0..layout.header.total_nodes as usize)
		.map(|node| layout.base + node * NATIVE_NODE_BYTES)
		.map(|start| Node::decode(&NATIVE[start..start + NATIVE_NODE_BYTES]).unwrap())
		.filter(|node| node.kind() == hfsplus_format::btree::Kind::Leaf)
		.find_map(|node| {
			node.records().find_map(|raw| {
				let record = Record::decode(raw, layout.volume.geometry).unwrap();
				if record.key.parent_or_thread != id {
					return None;
				}
				let Value::Thread { name, .. } = record.value else {
					return None;
				};
				let units_bytes = name.len() * size_of::<u16>();
				Some(offset_of(NATIVE, raw) + raw.len() - units_bytes)
			})
		})
		.unwrap();
	let first = &mut image[file_thread..file_thread + size_of::<u16>()];
	assert_eq!(first, &u16::from(b'R').to_be_bytes());
	first.copy_from_slice(&u16::from(b'r').to_be_bytes());
	let volume = Volume::open(image.as_slice()).unwrap();
	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	assert!(matches!(
		catalog.lookup(parent, STORED_FILE),
		Err(CatalogError::Corrupt {
			reason: Corruption::Thread,
			..
		})
	));
}

#[test]
fn rejects_a_leaf_chain_with_an_inconsistent_backward_link() {
	let layout = layout(NATIVE);
	let first = layout.header.first_leaf.unwrap();
	let root = layout.header.root.unwrap();
	let start = layout.base + first.0 as usize * NATIVE_NODE_BYTES;
	let node = Node::decode(&NATIVE[start..start + NATIVE_NODE_BYTES]).unwrap();
	let next = node.forward().unwrap();
	let mut image = NATIVE.to_vec();
	let back_link = layout.base + next.0 as usize * NATIVE_NODE_BYTES + NODE_BACK_LINK_OFFSET;
	image[back_link..back_link + size_of::<u32>()].copy_from_slice(&root.0.to_be_bytes());
	let volume = Volume::open(image.as_slice()).unwrap();
	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	let mut directory = catalog.read_dir(ROOT_ID).unwrap();
	assert!(matches!(
		directory.next_page(PAGE_SIZE),
		Err(CatalogError::Corrupt {
			reason: Corruption::NodeLinks,
			..
		})
	));
}

struct Layout {
	volume: Header,
	header: TreeHeader,
	base: usize,
}

fn layout(bytes: &[u8]) -> Layout {
	let start = PRIMARY_HEADER_OFFSET as usize;
	let volume = Header::decode(&bytes[start..start + HEADER_BYTES], bytes.len() as u64).unwrap();
	let fork = &volume.forks.catalog;
	let first = fork.extents().next().unwrap();
	let base = volume
		.geometry
		.block_range(first.start, first.blocks)
		.unwrap()
		.start as usize;
	let tree = Node::decode(&bytes[base..base + NATIVE_NODE_BYTES]).unwrap();
	let header = TreeHeader::decode(&tree, fork.logical_bytes).unwrap();
	Layout {
		volume,
		header,
		base,
	}
}

fn offset_of(whole: &[u8], slice: &[u8]) -> usize {
	slice.as_ptr() as usize - whole.as_ptr() as usize
}

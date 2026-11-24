//! H-READ-04 structural evidence from Apple-generated catalog and bitmap bytes.

use hfsplus_format::{
	btree::{Kind, Node, TreeHeader},
	catalog::{Key, Record, Value},
	volume::{
		AllocationBlock, CatalogId, FORK_BYTES, Fork, HEADER_BYTES, Header, PRIMARY_HEADER_OFFSET,
	},
};
use hfsplus_reader::{
	Volume,
	allocation::{AllocationError, AllocationLimits},
	catalog::{CatalogError, CatalogLimits, Corruption},
	check::{
		AllocationOwner, AlternateStatus, AttributeCoverage, CatalogProblem, CheckError,
		CheckLimit, CheckLimits,
	},
	overflow::ForkKind,
};

const OPTICAL: &[u8] = include_bytes!("fixtures/optical-volume.bin");
const MULTILEAF: &[u8] = include_bytes!("fixtures/native-catalog-volume.bin");
const NODE_BYTES: usize = 4096;
const OPTICAL_PAYLOAD_NAME: &str = "hello.txt";
const FILE_DATA_FORK_OFFSET: usize = 88;
const FILE_RESOURCE_FORK_OFFSET: usize = FILE_DATA_FORK_OFFSET + FORK_BYTES;
const HIGH_BIT: u8 = 1 << (u8::BITS - 1);
const BTREE_FORWARD_OFFSET: usize = 0;
const BTREE_BACKWARD_OFFSET: usize = size_of::<u32>();

#[test]
fn native_multilevel_catalog_and_allocation_counts_reconcile() {
	let volume = Volume::open(MULTILEAF).unwrap();
	let report = volume.check_structure(CheckLimits::default()).unwrap();
	assert_eq!(report.catalog_files, 97);
	assert_eq!(report.catalog_folders, 1);
	assert_eq!(report.total_blocks, 42);
	assert_eq!(report.metadata_blocks, 41);
	assert_eq!(report.reachable_fork_blocks, 1);
	assert_eq!(report.free_blocks, 0);
	assert_eq!(report.unclaimed_allocated_blocks, 0);
	assert_eq!(report.hard_link_groups, 0);
	assert_eq!(report.alternate, AlternateStatus::MatchesPrimary);
	assert_eq!(report.attributes, AttributeCoverage::Absent);
	assert!(report.orphan_extent_records_unchecked);
}

#[test]
fn detects_the_same_physical_claim_under_two_stored_fork_kinds() {
	let (header, record, id, data) = optical_file();
	let mut source = OPTICAL.to_vec();
	let start = record + FILE_RESOURCE_FORK_OFFSET;
	source[start..start + FORK_BYTES].copy_from_slice(&data.encode(header.geometry).unwrap());
	let volume = Volume::open(source.as_slice()).unwrap();
	assert!(
		volume
			.check_metadata_allocation(AllocationLimits::default())
			.is_ok()
	);
	let data_owner = AllocationOwner::FileFork {
		id,
		kind: ForkKind::Data,
	};
	let resource_owner = AllocationOwner::FileFork {
		id,
		kind: ForkKind::Resource,
	};
	assert!(matches!(
		volume.check_structure(CheckLimits::default()),
		Err(CheckError::Allocation(AllocationError::Overlap { first, second, .. }))
			if [first, second].contains(&data_owner) && [first, second].contains(&resource_owner)
	));
}

#[test]
fn reports_a_reachable_data_extent_marked_free() {
	let (header, _, id, payload) = optical_file();
	let block = payload.extents().next().unwrap().start;
	let bitmap = header.forks.allocation.extents().next().unwrap();
	let bitmap = header
		.geometry
		.block_range(bitmap.start, bitmap.blocks)
		.unwrap()
		.start as usize;
	let mut source = OPTICAL.to_vec();
	source[bitmap + (block.0 / u8::BITS) as usize] &= !(HIGH_BIT >> (block.0 % u8::BITS));
	let volume = Volume::open(source.as_slice()).unwrap();
	assert!(matches!(
		volume.check_structure(CheckLimits::default()),
		Err(CheckError::Allocation(AllocationError::MarkedFree {
			block: AllocationBlock(actual),
			owner: AllocationOwner::FileFork { id: owner, kind: ForkKind::Data },
		})) if actual == block.0 && owner == id
	));
}

#[test]
fn counts_unclaimed_allocations_without_certifying_orphan_semantics() {
	let (header, record, _, _) = optical_file();
	let mut source = OPTICAL.to_vec();
	let start = record + FILE_DATA_FORK_OFFSET;
	source[start..start + FORK_BYTES]
		.copy_from_slice(&Fork::empty().encode(header.geometry).unwrap());
	let volume = Volume::open(source.as_slice()).unwrap();
	let report = volume.check_structure(CheckLimits::default()).unwrap();
	assert_eq!(report.reachable_fork_blocks, 0);
	assert_eq!(report.unclaimed_allocated_blocks, 1);
	assert!(report.orphan_extent_records_unchecked);
}

#[test]
fn checks_catalog_counts_and_caps_the_audit() {
	let volume = Volume::open(MULTILEAF).unwrap();
	let limits = CheckLimits::default();
	assert!(matches!(
		volume.check_structure(CheckLimits {
			max_catalog_entries: 3,
			..limits
		}),
		Err(CheckError::Limit(CheckLimit::CatalogEntries))
	));
	assert!(matches!(
		volume.check_structure(CheckLimits {
			catalog: CatalogLimits {
				max_ancestors: 1,
				..limits.catalog
			},
			..limits
		}),
		Err(CheckError::Limit(CheckLimit::ParentDepth))
	));
	let mut header = *volume.header();
	let actual = header.files;
	header.files += 1;
	let mut source = MULTILEAF.to_vec();
	let start = PRIMARY_HEADER_OFFSET as usize;
	source[start..start + HEADER_BYTES].copy_from_slice(&header.encode().unwrap());
	let volume = Volume::open(source.as_slice()).unwrap();
	assert!(matches!(
		volume.check_structure(limits),
		Err(CheckError::Structure(CatalogProblem::FileCount { declared, actual: found }))
			if declared == actual + 1 && found == actual
	));
}

#[test]
fn detects_a_leaf_omitted_from_the_chain_but_reachable_from_the_index() {
	let volume = Volume::open(MULTILEAF).unwrap();
	let volume_header = volume.header();
	let fork = &volume_header.forks.catalog;
	let extent = fork.extents().next().unwrap();
	let base = volume_header
		.geometry
		.block_range(extent.start, extent.blocks)
		.unwrap()
		.start as usize;
	let header = Node::decode(&MULTILEAF[base..base + NODE_BYTES]).unwrap();
	let tree = TreeHeader::decode(&header, fork.logical_bytes).unwrap();
	let first = tree.first_leaf.unwrap();
	let start = |id: hfsplus_format::btree::NodeId| base + id.0 as usize * NODE_BYTES;
	let node = |id| Node::decode(&MULTILEAF[start(id)..start(id) + NODE_BYTES]).unwrap();
	let omitted = node(first).forward().unwrap();
	let next = node(omitted).forward().unwrap();
	let mut source = MULTILEAF.to_vec();
	let forward = start(first) + BTREE_FORWARD_OFFSET;
	let backward = start(next) + BTREE_BACKWARD_OFFSET;
	source[forward..forward + size_of::<u32>()].copy_from_slice(&next.0.to_be_bytes());
	source[backward..backward + size_of::<u32>()].copy_from_slice(&first.0.to_be_bytes());
	let volume = Volume::open(source.as_slice()).unwrap();
	assert!(matches!(
		volume.check_structure(CheckLimits::default()),
		Err(CheckError::Catalog(CatalogError::Corrupt { node, reason: Corruption::NodeLinks }))
			if node == omitted
	));
}

fn optical_file() -> (Header, usize, CatalogId, Fork) {
	let volume = Volume::open(OPTICAL).unwrap();
	let header = *volume.header();
	let geometry = header.geometry;
	let catalog = &header.forks.catalog;
	let start = catalog.extents().next().unwrap();
	let base = geometry
		.block_range(start.start, start.blocks)
		.unwrap()
		.start as usize;
	let first = Node::decode(&OPTICAL[base..base + NODE_BYTES]).unwrap();
	let tree = TreeHeader::decode(&first, catalog.logical_bytes).unwrap();
	let start = base + NODE_BYTES * tree.first_leaf.unwrap().0 as usize;
	let leaf = Node::decode(&OPTICAL[start..start + NODE_BYTES]).unwrap();
	assert_eq!(leaf.kind(), Kind::Leaf);
	for raw in leaf.records() {
		let entry = Record::decode(raw, geometry).unwrap();
		if entry
			.key
			.name
			.units()
			.eq(OPTICAL_PAYLOAD_NAME.encode_utf16())
			&& let Value::File { metadata, data, .. } = entry.value
		{
			let (_, value) = Key::split_leaf(raw).unwrap();
			let offset = value.as_ptr() as usize - OPTICAL.as_ptr() as usize;
			return (header, offset, metadata.id, data);
		}
	}
	panic!("missing native optical payload");
}

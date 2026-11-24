//! Native HFS+ hard-link identity and fork coverage for H-READ-01 and H-V04.

use std::num::NonZeroUsize;

use hfsplus_core::{ByteLen, ByteOffset, ByteRange, RangeError, ReadAt};
use hfsplus_format::{
	btree::{Kind, Node, TreeHeader},
	catalog::{Key, Record, Value},
	hard_links::{LinkReference, PRIVATE_DIRECTORY_NAME},
	volume::{CatalogId, PRIMARY_HEADER_OFFSET},
};
use hfsplus_reader::{
	Volume,
	catalog::{
		Catalog, CatalogLimits, HardLinkError, HardLinkLimits, HardLinkProfile, LinkCorruption,
		LinkTarget, ROOT_ID, RegularFile,
	},
	overflow::{ForkKind, OverflowLimits},
};

const NATIVE: &[u8] = include_bytes!("fixtures/native-chained-hardlinks.bin");
const CHAIN_FREE: &[u8] = include_bytes!("fixtures/native-chain-free-hardlinks.bin");
const OPTICAL: &[u8] = include_bytes!("fixtures/optical-volume.bin");
const FIRST_INODE: CatalogId = CatalogId(19);
const SECOND_INODE: CatalogId = CatalogId(23);
const PRIVATE_DIRECTORY: CatalogId = CatalogId(16);
const FIRST_STUB: CatalogId = CatalogId(20);
const FIRST_PAYLOAD: &[u8] = b"pomologie linked file\n";
const FIRST_RESOURCE: &[u8] = b"pomologie resource\0fork\n";
const SECOND_PAYLOAD: &[u8] = b"pomologie second group\n";
const NATIVE_OWNER: u32 = 501;
const NATIVE_GROUP: u32 = 20;
const REGULAR_MODE: u16 = 0o100000;
const FIRST_MODE: u16 = REGULAR_MODE | 0o751;
const SECOND_MODE: u16 = REGULAR_MODE | 0o640;
const MODIFIED_DATE_INDEX: usize = 1;
const HFS_UNIX_EPOCH_SECONDS: u32 = 2_082_844_800;
const FIRST_MODIFIED_UNIX_SECONDS: u32 = 1_704_195_296;
const NATIVE_NODE_BYTES: usize = 4096;
const STUB_FLAGS_OFFSET: usize = 2;
const STUB_PREVIOUS_OFFSET: usize = 32;
const STUB_REFERENCE_OFFSET: usize = 44;
const STUB_FINDER_OFFSET: usize = 48;
const HAS_LINK_CHAIN: u16 = 0x0020;
const TEST_MISSING_REFERENCE: u32 = 777;
const ENTRY_LIMIT: NonZeroUsize = NonZeroUsize::new(6).unwrap();

#[path = "hard_links/overflow_owner.rs"]
mod overflow_owner;

#[test]
fn native_and_chain_free_aliases_resolve_identity_metadata_and_distinct_forks() {
	for (fixture, profile) in [
		(NATIVE, HardLinkProfile::VerifiedChain),
		(CHAIN_FREE, HardLinkProfile::ChainFree),
	] {
		let volume = Volume::open(Fixture::decode(fixture)).unwrap();
		let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
		let nested = named(&catalog, ROOT_ID, "nested");
		let aliases = [(ROOT_ID, "one"), (ROOT_ID, "two"), (nested, "three")];
		let resolved: Vec<_> = aliases
			.into_iter()
			.map(|(parent, name)| {
				let id = named(&catalog, parent, name);
				let file = catalog
					.regular_file(id, HardLinkLimits::default())
					.unwrap()
					.unwrap();
				assert_eq!(file.id(), id);
				assert_eq!(file.parent(), parent);
				assert!(file.name().iter().copied().eq(name.encode_utf16()));
				assert_ne!(file.id(), file.inode_id());
				assert_eq!(file.inode_id(), FIRST_INODE);
				assert_eq!(file.metadata().permissions.uid, NATIVE_OWNER);
				assert_eq!(file.metadata().permissions.gid, NATIVE_GROUP);
				assert_eq!(file.metadata().permissions.mode, FIRST_MODE);
				assert_eq!(
					file.metadata().dates[MODIFIED_DATE_INDEX],
					HFS_UNIX_EPOCH_SECONDS + FIRST_MODIFIED_UNIX_SECONDS
				);
				let link = file.hard_link().unwrap();
				assert_eq!(link.count.get(), 3);
				assert_eq!(link.profile, profile);
				assert_storage(&file, ForkKind::Data, FIRST_PAYLOAD);
				assert_storage(&file, ForkKind::Resource, FIRST_RESOURCE);
				(file.id(), link.reference)
			})
			.collect();
		for pair in resolved.windows(2) {
			assert_ne!(pair[0].0, pair[1].0);
			assert_eq!(pair[0].1, pair[1].1);
		}
		for (parent, name) in [(ROOT_ID, "four"), (nested, "five")] {
			let id = named(&catalog, parent, name);
			let file = catalog
				.regular_file(id, HardLinkLimits::default())
				.unwrap()
				.unwrap();
			assert_eq!(file.inode_id(), SECOND_INODE);
			assert_eq!(file.metadata().permissions.mode, SECOND_MODE);
			let link = file.hard_link().unwrap();
			assert_ne!(link.reference, resolved[0].1);
			assert_eq!(link.count.get(), 2);
			assert_eq!(link.profile, profile);
			assert_storage(&file, ForkKind::Data, SECOND_PAYLOAD);
			assert_storage(&file, ForkKind::Resource, b"");
		}

		assert!(
			catalog
				.lookup_raw(ROOT_ID, &PRIVATE_DIRECTORY_NAME)
				.unwrap()
				.is_none()
		);
		for id in [PRIVATE_DIRECTORY, FIRST_INODE, SECOND_INODE] {
			assert!(
				catalog
					.regular_file(id, HardLinkLimits::default())
					.unwrap()
					.is_none()
			);
		}
		let mut root = catalog.read_dir(ROOT_ID).unwrap();
		let page = root.next_page(ENTRY_LIMIT).unwrap();
		assert!(page.complete);
		let names: Vec<_> = page
			.entries
			.iter()
			.map(|entry| String::from_utf16(&entry.name).unwrap())
			.collect();
		assert_eq!(names, ["four", "nested", "one", "two"]);
	}
}

#[test]
fn direct_file_storage_uses_the_visible_catalog_id() {
	let volume = Volume::open(OPTICAL).unwrap();
	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	let file = catalog.lookup(ROOT_ID, "hello.txt").unwrap().unwrap();
	let regular = catalog
		.regular_file(file.metadata.id, HardLinkLimits::default())
		.unwrap()
		.unwrap();
	assert_eq!(regular.id(), regular.inode_id());
	assert!(regular.hard_link().is_none());
	assert_storage(&regular, ForkKind::Data, b"pomologie fixture\n");
	assert!(matches!(
		catalog.regular_file(ROOT_ID, HardLinkLimits::default()),
		Err(HardLinkError::NotRegular(ROOT_ID))
	));
}

#[test]
fn dangling_inodes_and_unqualified_directory_links_have_distinct_errors() {
	let mut fixture = Fixture::decode(NATIVE);
	let record = value_offset(&fixture, FIRST_STUB);
	fixture.replace(
		record + STUB_REFERENCE_OFFSET as u64,
		TEST_MISSING_REFERENCE.to_be_bytes(),
	);
	let volume = Volume::open(fixture).unwrap();
	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	assert!(matches!(
		catalog.regular_file(FIRST_STUB, HardLinkLimits::default()),
		Err(HardLinkError::Dangling {
			id: FIRST_STUB,
			target: LinkTarget::Inode(reference),
		}) if reference == LinkReference::new(TEST_MISSING_REFERENCE).unwrap()
	));

	let mut fixture = Fixture::decode(NATIVE);
	let record = value_offset(&fixture, FIRST_STUB);
	fixture.replace(record + STUB_FINDER_OFFSET as u64, *b"fdrpMACS");
	let volume = Volume::open(fixture).unwrap();
	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	assert!(matches!(
		catalog.regular_file(FIRST_STUB, HardLinkLimits::default()),
		Err(HardLinkError::DirectoryLink(FIRST_STUB))
	));
}

#[test]
fn full_chain_requires_reciprocal_neighbors_and_consistent_profiles() {
	let mut fixture = Fixture::decode(NATIVE);
	let record = value_offset(&fixture, FIRST_STUB);
	fixture.replace(record + STUB_PREVIOUS_OFFSET as u64, 0u32.to_be_bytes());
	let volume = Volume::open(fixture).unwrap();
	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	assert!(matches!(
		catalog.regular_file(FIRST_STUB, HardLinkLimits::default()),
		Err(HardLinkError::Corrupt {
			id: FIRST_STUB,
			reason: LinkCorruption::Chain
		})
	));

	let mut fixture = Fixture::decode(NATIVE);
	let record = value_offset(&fixture, FIRST_STUB);
	let flags = record + STUB_FLAGS_OFFSET as u64;
	let mut value = [0; size_of::<u16>()];
	fixture
		.read_exact_at(ByteOffset(flags), &mut value)
		.unwrap();
	fixture.replace(
		flags,
		(u16::from_be_bytes(value) & !HAS_LINK_CHAIN).to_be_bytes(),
	);
	let volume = Volume::open(fixture).unwrap();
	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	assert!(matches!(
		catalog.regular_file(FIRST_STUB, HardLinkLimits::default()),
		Err(HardLinkError::Corrupt {
			id: FIRST_STUB,
			reason: LinkCorruption::ConflictingProfiles
		})
	));
}

#[test]
fn configured_chain_budget_never_upgrades_chain_free_counts() {
	let limits = HardLinkLimits { max_chain_links: 2 };
	let volume = Volume::open(Fixture::decode(NATIVE)).unwrap();
	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	assert!(matches!(
		catalog.regular_file(FIRST_STUB, limits),
		Err(HardLinkError::ChainLimit(FIRST_STUB))
	));

	let volume = Volume::open(Fixture::decode(CHAIN_FREE)).unwrap();
	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	let link = catalog
		.regular_file(FIRST_STUB, limits)
		.unwrap()
		.unwrap()
		.hard_link()
		.unwrap();
	assert_eq!(link.count.get(), 3);
	assert_eq!(link.profile, HardLinkProfile::ChainFree);
}

#[test]
fn checks_shared_storage_once_and_reports_unqualified_attribute_layout() {
	use hfsplus_reader::check::{AttributeCoverage, CheckLimits};

	let volume = Volume::open(ownership_fixture(NATIVE)).unwrap();
	let report = volume.check_structure(CheckLimits::default()).unwrap();
	assert_eq!(report.hard_link_groups, 2);
	assert_eq!(report.reachable_fork_blocks, 3);
	assert_eq!(report.metadata_blocks, 99);
	assert_eq!(report.free_blocks, 3994);
	assert_eq!(report.unclaimed_allocated_blocks, 0);
	assert_eq!(report.attributes, AttributeCoverage::UnsupportedTree);
}

#[test]
fn structural_check_rejects_a_chain_free_count_that_disagrees_with_all_visible_aliases() {
	use hfsplus_reader::check::{CatalogProblem, CheckError, CheckLimits};

	const INODE_LINK_COUNT_OFFSET: u64 = 44;
	let mut fixture = ownership_fixture(CHAIN_FREE);
	let inode = value_offset(&fixture, FIRST_INODE);
	fixture.replace(inode + INODE_LINK_COUNT_OFFSET, 2u32.to_be_bytes());
	let volume = Volume::open(fixture).unwrap();
	assert!(matches!(
		volume.check_structure(CheckLimits::default()),
		Err(CheckError::Structure(CatalogProblem::LinkCount {
			declared: 2,
			actual: 3,
			..
		}))
	));
}

fn ownership_fixture(raw: &[u8]) -> Fixture {
	const BITMAP: &[u8] = include_bytes!("fixtures/native-hardlink-allocation.bin");
	const UNQUALIFIED_NODE_BYTES: u16 = 4096;
	const TREE_PREFIX_BYTES: usize = 34;
	const TREE_NODE_KIND_OFFSET: usize = 8;
	const TREE_NODE_SIZE_OFFSET: usize = TREE_PREFIX_BYTES - size_of::<u16>();

	let mut fixture = Fixture::decode(raw);
	let header = *Volume::open(&fixture).unwrap().header();
	// Both native payloads are shorter than a volume header. Omitting them
	// proves the structural check does not read or authenticate file bytes.
	fixture
		.slices
		.retain(|(_, bytes)| bytes.len() >= hfsplus_format::volume::HEADER_BYTES);
	let start = |fork: &hfsplus_format::volume::Fork| {
		let extent = fork.extents().next().unwrap();
		header
			.geometry
			.block_range(extent.start, extent.blocks)
			.unwrap()
			.start
	};
	fixture
		.slices
		.push((start(&header.forks.allocation), BITMAP.to_vec()));
	let mut unsupported_attributes = [0; TREE_PREFIX_BYTES];
	unsupported_attributes[TREE_NODE_KIND_OFFSET] = Kind::Header as u8;
	unsupported_attributes[TREE_NODE_SIZE_OFFSET..]
		.copy_from_slice(&UNQUALIFIED_NODE_BYTES.to_be_bytes());
	fixture.slices.push((
		start(&header.forks.attributes),
		unsupported_attributes.to_vec(),
	));
	fixture
}

fn named<S: ReadAt>(catalog: &Catalog<'_, S>, parent: CatalogId, name: &str) -> CatalogId
where
	S::Error: std::fmt::Debug,
{
	catalog.lookup(parent, name).unwrap().unwrap().metadata.id
}

fn assert_storage<S: ReadAt>(file: &RegularFile<'_, S>, kind: ForkKind, expected: &[u8])
where
	S::Error: std::fmt::Debug,
{
	let fork = file
		.open_stored_fork(kind, OverflowLimits::default())
		.unwrap();
	assert_eq!(fork.len(), expected.len().into());
	let mut observed = vec![0; expected.len()];
	fork.read_exact_at(ByteOffset(0), &mut observed).unwrap();
	assert_eq!(observed, expected);
}

fn value_offset(fixture: &Fixture, id: CatalogId) -> u64 {
	let volume = Volume::open(fixture).unwrap();
	let catalog = &volume.header().forks.catalog;
	let geometry = volume.header().geometry;
	let first_extent = catalog.extents().next().unwrap();
	let base = geometry
		.block_range(first_extent.start, first_extent.blocks)
		.unwrap()
		.start;
	let mut bytes = [0; NATIVE_NODE_BYTES];
	fixture.read_exact_at(ByteOffset(base), &mut bytes).unwrap();
	let header = TreeHeader::decode(&Node::decode(&bytes).unwrap(), catalog.logical_bytes).unwrap();
	assert_eq!(usize::from(header.node_bytes), NATIVE_NODE_BYTES);
	for node in
		(1..header.total_nodes).map(|node| base + u64::from(node) * NATIVE_NODE_BYTES as u64)
	{
		if fixture.read_exact_at(ByteOffset(node), &mut bytes).is_err() {
			continue;
		}
		let tree_node = Node::decode(&bytes).unwrap();
		if tree_node.kind() != Kind::Leaf {
			continue;
		}
		for raw in tree_node.records() {
			let entry = Record::decode(raw, geometry).unwrap();
			if matches!(entry.value, Value::File { metadata, .. } if metadata.id == id) {
				let (_, value) = Key::split_leaf(raw).unwrap();
				return node + (value.as_ptr() as usize - bytes.as_ptr() as usize) as u64;
			}
		}
	}
	panic!("missing native record {}", id.0);
}

#[derive(Debug)]
struct Fixture {
	len: ByteLen,
	slices: Vec<(u64, Vec<u8>)>,
}

impl Fixture {
	fn decode(mut bytes: &[u8]) -> Self {
		fn take<const N: usize>(data: &mut &[u8]) -> [u8; N] {
			let (first, rest) = data.split_first_chunk().unwrap();
			*data = rest;
			*first
		}
		let len = ByteLen(u64::from_be_bytes(take(&mut bytes)));
		let count = u16::from_be_bytes(take(&mut bytes));
		let slices = (0..count)
			.map(|_| {
				let start = u64::from_be_bytes(take(&mut bytes));
				let width = u32::from_be_bytes(take(&mut bytes)) as usize;
				let (slice, rest) = bytes.split_at(width);
				bytes = rest;
				(start, slice.to_vec())
			})
			.collect();
		assert!(bytes.is_empty());
		assert!(len.0 > PRIMARY_HEADER_OFFSET);
		Self { len, slices }
	}

	fn replace<const N: usize>(&mut self, offset: u64, value: [u8; N]) {
		let bytes = self
			.slices
			.iter_mut()
			.find_map(|(start, data)| {
				let relative = usize::try_from(offset.checked_sub(*start)?).ok()?;
				data.get_mut(relative..relative.checked_add(N)?)
			})
			.unwrap();
		bytes.copy_from_slice(&value);
	}
}

impl ReadAt for Fixture {
	type Error = RangeError;

	fn len(&self) -> ByteLen {
		self.len
	}

	fn read_exact_at(&self, offset: ByteOffset, output: &mut [u8]) -> Result<(), Self::Error> {
		ByteRange::new(offset, output.len().into(), self.len)?;
		let bytes = self
			.slices
			.iter()
			.find_map(|(start, data)| {
				let relative = usize::try_from(offset.0.checked_sub(*start)?).ok()?;
				data.get(relative..relative.checked_add(output.len())?)
			})
			.ok_or(RangeError::OutsideSource)?;
		output.copy_from_slice(bytes);
		Ok(())
	}
}

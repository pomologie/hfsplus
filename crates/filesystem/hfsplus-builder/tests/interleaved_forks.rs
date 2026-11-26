//! The opt-in layout emits genuinely fragmented physical extents for both forks.

use std::{
	env,
	fs::File,
	num::{NonZeroU32, NonZeroUsize},
};

use hfsplus_builder::{
	PlanError, SealedPlan,
	volume::{
		CapacityError, CapacityPolicy, EntryKind, EntryMetadata, ForkSource, HardLinkGroup,
		ManifestEntry, VolumeError, VolumeOptions, plan, plan_with_hard_links,
	},
};
use hfsplus_core::{ByteLen, ByteOffset, RangeError, ReadAt};
use hfsplus_format::{
	btree::{Node, TreeHeader},
	extents::{EXTENTS_PER_RECORD, ForkKind},
	volume::{CatalogId, Geometry},
};
use hfsplus_reader::{SpecialFile, Volume};

const LABEL: &str = "Interleaved Forks";
const DIRECTORY: &str = "More";
const FILE_NAME: &str = "Paired.bin";
const ITEM_COUNT: usize = 64;
const BLOCK_BYTES: usize = 4096;
const RUN_BLOCKS: u32 = 2;
const RUN_COUNT: u32 = 20;
const FORK_BLOCKS: u32 = RUN_BLOCKS * RUN_COUNT;
const DATA_BYTES: usize = (FORK_BLOCKS as usize - 1) * BLOCK_BYTES + 17;
const RESOURCE_BYTES: usize = (FORK_BLOCKS as usize - 1) * BLOCK_BYTES + 31;
const DATA_SEED: u8 = 17;
const RESOURCE_SEED: u8 = 81;
const SOURCE_PREFIX: &[u8] = b"discarded source prefix";
const IMAGE_BYTES: u64 = 8 * 1024 * 1024;
const OUTPUT_ENV: &str = "POMOLOGIE_INTERLEAVED_FORK_OUTPUT";

// An owned source without Clone ensures the builder admits each source once.
struct Source<'a>(&'a [u8]);

impl ReadAt for Source<'_> {
	type Error = RangeError;

	fn len(&self) -> ByteLen {
		self.0.len().into()
	}

	fn read_exact_at(&self, offset: ByteOffset, output: &mut [u8]) -> Result<(), Self::Error> {
		self.0.read_exact_at(offset, output)
	}
}

fn pattern(length: usize, seed: u8) -> Vec<u8> {
	SOURCE_PREFIX
		.iter()
		.copied()
		.chain((0..length).map(|offset| seed + u8::try_from(offset / BLOCK_BYTES).unwrap()))
		.collect()
}

fn data_source(data: &[u8]) -> ForkSource<Source<'_>> {
	ForkSource::new(
		Source(data),
		ByteOffset(SOURCE_PREFIX.len() as u64),
		ByteLen::from(data.len() - SOURCE_PREFIX.len()),
	)
	.unwrap()
}

fn options() -> VolumeOptions<'static> {
	let geometry = Geometry::new(
		BLOCK_BYTES as u32,
		(IMAGE_BYTES / BLOCK_BYTES as u64) as u32,
		IMAGE_BYTES,
	)
	.unwrap();
	VolumeOptions::new(LABEL, geometry)
}

fn image<'a>(
	data: &'a [u8],
	resource: &'a [u8],
	max_ranges: usize,
) -> Result<SealedPlan<Source<'a>>, CapacityError> {
	let names: Vec<_> = (0..ITEM_COUNT)
		.map(|index| format!("{DIRECTORY}/item-{index:02}"))
		.collect();
	let mut entries: Vec<_> = names
		.iter()
		.map(|path| ManifestEntry {
			path,
			metadata: EntryMetadata::new(0o644),
			kind: EntryKind::File {
				data: ForkSource::whole(Source(&[])),
				resource: None,
			},
		})
		.collect();
	entries.extend([
		ManifestEntry {
			path: DIRECTORY,
			metadata: EntryMetadata::new(0o755),
			kind: EntryKind::Folder,
		},
		ManifestEntry {
			path: FILE_NAME,
			metadata: EntryMetadata::new(0o644),
			kind: EntryKind::File {
				data: data_source(data),
				resource: Some(data_source(resource)),
			},
		},
	]);
	let mut options = options();
	options.limits.max_ranges = max_ranges;
	let mut draft = plan(options, entries).unwrap();
	draft.interleave_paired_forks(NonZeroU32::new(RUN_BLOCKS).unwrap());
	draft.seal_with_capacity(CapacityPolicy::Exact {
		bytes: ByteLen(IMAGE_BYTES),
		free_space: ByteLen(0),
	})
}

#[test]
fn writes_a_multilevel_catalog_and_resolves_all_fragmented_fork_bytes() {
	let data = pattern(DATA_BYTES, DATA_SEED);
	let resource = pattern(RESOURCE_BYTES, RESOURCE_SEED);
	let image = image(&data, &resource, usize::MAX).unwrap();
	let volume = Volume::open(&image).unwrap();
	let report = volume.check_structure(Default::default()).unwrap();
	assert_eq!(report.reachable_fork_blocks, 2 * FORK_BLOCKS);
	assert_eq!(report.unclaimed_allocated_blocks, 0);
	let catalog_fork = volume.open_special_fork(SpecialFile::Catalog).unwrap();
	let mut bytes = [0; BLOCK_BYTES];
	catalog_fork
		.read_exact_at(ByteOffset(0), &mut bytes)
		.unwrap();
	let tree = TreeHeader::decode(&Node::decode(&bytes).unwrap(), catalog_fork.len().0).unwrap();
	assert!(tree.depth > 1);
	assert_eq!(tree.leaf_records, (2 * (ITEM_COUNT + 3)) as u32);

	let catalog = volume.open_catalog(Default::default()).unwrap();
	let root = catalog.root().unwrap().metadata.id;
	let directory = catalog
		.lookup(root, DIRECTORY)
		.unwrap()
		.unwrap()
		.metadata
		.id;
	let mut seen = 0;
	let mut scan = catalog.read_dir(directory).unwrap();
	const PAGE_SIZE: usize = 17;
	loop {
		let page = scan
			.next_page(NonZeroUsize::new(PAGE_SIZE).unwrap())
			.unwrap();
		for entry in page.entries {
			let name = format!("item-{seen:02}");
			assert_eq!(
				catalog.lookup(directory, &name).unwrap(),
				Some(entry.clone())
			);
			assert_eq!(catalog.entry(entry.metadata.id).unwrap(), Some(entry));
			seen += 1;
		}
		if page.complete {
			break;
		}
	}
	assert_eq!(seen, ITEM_COUNT);
	let file = catalog.lookup(root, FILE_NAME).unwrap().unwrap();
	assert_eq!(catalog.entry(file.metadata.id).unwrap(), Some(file.clone()));
	assert_eq!(
		file.metadata.id,
		CatalogId(CatalogId::FIRST_USER.0 + ITEM_COUNT as u32 + 1)
	);
	let stored = catalog
		.regular_file(file.metadata.id, Default::default())
		.unwrap()
		.unwrap();
	let hfsplus_reader::catalog::EntryKind::File {
		data: data_record,
		resource: resource_record,
	} = file.kind
	else {
		panic!("expected a file record");
	};
	for (kind, embedded, expected) in [
		(ForkKind::Data, data_record, &data[SOURCE_PREFIX.len()..]),
		(
			ForkKind::Resource,
			resource_record,
			&resource[SOURCE_PREFIX.len()..],
		),
	] {
		assert_eq!(embedded.extents().count(), EXTENTS_PER_RECORD);
		assert!(embedded.has_overflow());
		let fork = stored.open_stored_fork(kind, Default::default()).unwrap();
		assert_eq!(fork.extents().count(), RUN_COUNT as usize);
		let mut actual = vec![0; expected.len()];
		fork.read_exact_at(ByteOffset(0), &mut actual).unwrap();
		assert_eq!(actual, expected);
	}
}

#[test]
fn bounds_the_number_of_paired_runs_before_emission() {
	let data = pattern(DATA_BYTES, DATA_SEED);
	let resource = pattern(RESOURCE_BYTES, RESOURCE_SEED);
	assert!(matches!(
		image(&data, &resource, (2 * RUN_COUNT - 1) as usize),
		Err(CapacityError::Volume(VolumeError::Plan(
			PlanError::RangeLimit
		)))
	));
}

#[test]
fn coalesces_the_unpaired_tail_and_stores_overflow_under_the_hard_link_inode() {
	const RESOURCE_BLOCKS: u32 = EXTENTS_PER_RECORD as u32 * RUN_BLOCKS;
	const ALIASES: [&str; 2] = ["First", "Second"];
	let data = pattern(DATA_BYTES, DATA_SEED);
	let resource = pattern(RESOURCE_BLOCKS as usize * BLOCK_BYTES, RESOURCE_SEED);
	let group = HardLinkGroup {
		paths: ALIASES.to_vec(),
		metadata: EntryMetadata::new(0o644),
		data: data_source(&data),
		resource: Some(data_source(&resource)),
	};
	let mut draft = plan_with_hard_links(options(), Vec::new(), vec![group]).unwrap();
	draft.interleave_paired_forks(NonZeroU32::new(RUN_BLOCKS).unwrap());
	let inode = draft.inode_id(ALIASES[0]).unwrap();
	let image = draft.seal().unwrap();
	let volume = Volume::open(&image).unwrap();
	let report = volume.check_structure(Default::default()).unwrap();
	assert_eq!(report.hard_link_groups, 1);
	assert_eq!(report.reachable_fork_blocks, FORK_BLOCKS + RESOURCE_BLOCKS);
	let catalog = volume.open_catalog(Default::default()).unwrap();
	let root = catalog.root().unwrap().metadata.id;
	for alias in ALIASES {
		let entry = catalog.lookup(root, alias).unwrap().unwrap();
		let file = catalog
			.regular_file(entry.metadata.id, Default::default())
			.unwrap()
			.unwrap();
		assert_ne!(entry.metadata.id, inode);
		assert_eq!(file.inode_id(), inode);
		let fork = file
			.open_stored_fork(ForkKind::Data, Default::default())
			.unwrap();
		assert_eq!(fork.extents().count(), EXTENTS_PER_RECORD + 1);
		assert_eq!(
			fork.extents().last().unwrap().blocks,
			FORK_BLOCKS - RESOURCE_BLOCKS
		);
		let mut actual = vec![0; DATA_BYTES];
		fork.read_exact_at(ByteOffset(0), &mut actual).unwrap();
		assert_eq!(actual, data[SOURCE_PREFIX.len()..]);
		let fork = file
			.open_stored_fork(ForkKind::Resource, Default::default())
			.unwrap();
		assert_eq!(fork.extents().count(), EXTENTS_PER_RECORD);
	}
}

#[test]
#[ignore = "emits a disposable image for the independent reader and macOS checks"]
fn emit_interleaved_forks() {
	let data = pattern(DATA_BYTES, DATA_SEED);
	let resource = pattern(RESOURCE_BYTES, RESOURCE_SEED);
	let image = image(&data, &resource, usize::MAX).unwrap();
	image
		.write_to(File::create_new(env::var_os(OUTPUT_ENV).unwrap()).unwrap())
		.unwrap();
}

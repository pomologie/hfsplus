//! Exact-layout checks for H-BUILD-02 and the allocation budget in H-V01.

use std::convert::Infallible;

use hfsplus_builder::{
	SealedPlan,
	volume::{
		CapacityError, CapacityPolicy, EntryKind, EntryMetadata, ForkSource, ManifestEntry,
		VolumeDraft, VolumeError, VolumeOptions, plan,
	},
};
use hfsplus_core::{ByteLen, ByteOffset, ReadAt};
use hfsplus_format::volume::{
	ALTERNATE_HEADER_FROM_END, HEADER_BYTES, Header, PRIMARY_HEADER_OFFSET,
};

const LABEL: &str = "Capacity";
const LARGE_BLOCK: u32 = 4096;
const SECTOR: u64 = 512;
const NODE_BYTES: u64 = 4096;
const EMPTY_TREE_NODES: u64 = 3;
const BOOT_AND_HEADER_BLOCKS: u64 = 2;
const BITMAP_BLOCKS: u64 = 1;
const BYTES_PER_GIB: u64 = 1 << 30;
const DATA_PATH: &str = "Document";
const LINK_PATH: &str = "Application Link";
const GENERATED_PATH: &str = ".DS_Store";

#[derive(Debug)]
struct NeverRead(ByteLen);

impl ReadAt for NeverRead {
	type Error = Infallible;

	fn len(&self) -> ByteLen {
		self.0
	}

	fn read_exact_at(&self, _: ByteOffset, _: &mut [u8]) -> Result<(), Self::Error> {
		panic!("measuring or sealing a draft must not read the source payload")
	}
}

fn auto(free_bytes: u64) -> CapacityPolicy {
	CapacityPolicy::Auto {
		free_space: ByteLen(free_bytes),
	}
}

fn blank_options(block_size: u32) -> VolumeOptions<'static> {
	VolumeOptions::for_capacity(LABEL, block_size).unwrap()
}

fn file(data: u64, resource: Option<u64>) -> ManifestEntry<'static, NeverRead> {
	ManifestEntry {
		path: DATA_PATH,
		metadata: EntryMetadata::new(0o644),
		kind: EntryKind::File {
			data: ForkSource::whole(NeverRead(ByteLen(data))),
			resource: resource.map(|bytes| ForkSource::whole(NeverRead(ByteLen(bytes)))),
		},
	}
}

fn header(image: &SealedPlan<NeverRead>, offset: u64) -> Header {
	let mut bytes = [0; HEADER_BYTES];
	image.read_exact_at(ByteOffset(offset), &mut bytes).unwrap();
	Header::decode(&bytes, image.image_len().0).unwrap()
}

#[test]
fn rounds_each_fork_and_reserve_and_preserves_an_exact_partial_tail() {
	let block = u64::from(LARGE_BLOCK);
	let inputs = || {
		vec![
			file(block + 1, Some(1)),
			ManifestEntry {
				path: LINK_PATH,
				metadata: EntryMetadata::new(0o777),
				kind: EntryKind::Symlink {
					target: b"/Applications".to_vec(),
				},
			},
		]
	};
	let occupied_blocks = BOOT_AND_HEADER_BLOCKS + BITMAP_BLOCKS + EMPTY_TREE_NODES + 2 + 1 + 1;
	let mut draft = plan(blank_options(LARGE_BLOCK), inputs()).unwrap();
	let smallest = draft.capacity(auto(1)).unwrap();
	assert_eq!(
		smallest.image_bytes(),
		ByteLen((occupied_blocks + 1) * block)
	);
	assert_eq!(smallest.free_bytes(), ByteLen(block));

	let exact = CapacityPolicy::Exact {
		bytes: ByteLen(smallest.image_bytes().0 + SECTOR),
		free_space: ByteLen(1),
	};
	let selected = draft.capacity(exact).unwrap();
	assert_eq!(selected.minimum_bytes(), smallest.image_bytes());
	assert_eq!(selected.free_bytes(), ByteLen(block));
	assert_eq!(
		selected.image_bytes(),
		ByteLen(smallest.image_bytes().0 + SECTOR)
	);
	let image = draft.seal_with_capacity(exact).unwrap();
	assert_eq!(image.image_len(), selected.image_bytes());
	let primary = header(&image, PRIMARY_HEADER_OFFSET);
	let alternate = header(&image, selected.geometry().alternate_header_offset());
	assert_eq!(primary.geometry, selected.geometry());
	assert_eq!(primary.free_blocks, 1);
	assert_eq!(primary, alternate);
	assert_eq!(primary.files, 2);
}

#[test]
fn shrink_limit_rejects_the_sector_below_the_minimum() {
	let mut draft = plan(blank_options(LARGE_BLOCK), vec![file(11, None)]).unwrap();
	let smallest = draft.capacity(auto(0)).unwrap();
	let short = ByteLen(smallest.image_bytes().0 - SECTOR);
	let policy = CapacityPolicy::ShrinkWithin {
		max_bytes: short,
		free_space: ByteLen(0),
	};
	assert!(matches!(
		draft.capacity(policy),
		Err(CapacityError::InsufficientSpace { required, available })
			if required == smallest.image_bytes() && available == short
	));
	let policy = CapacityPolicy::ShrinkWithin {
		max_bytes: ByteLen(smallest.image_bytes().0 + SECTOR),
		free_space: ByteLen(0),
	};
	assert_eq!(draft.capacity(policy).unwrap(), smallest);
	let bad_exact = CapacityPolicy::Exact {
		bytes: ByteLen(smallest.image_bytes().0 + 1),
		free_space: ByteLen(0),
	};
	assert!(matches!(
		draft.capacity(bad_exact),
		Err(CapacityError::InvalidSize(_))
	));
	assert!(matches!(
		draft.capacity(auto(u64::MAX)),
		Err(CapacityError::AllocationLimit)
	));
}

#[test]
fn seals_at_exact_capacity_and_rejects_one_allocation_block_less() {
	const PAYLOAD_BYTES: u64 = 11;
	const FILE_BLOCKS: u64 = 1;
	let block = u64::from(LARGE_BLOCK);
	let occupied = BOOT_AND_HEADER_BLOCKS + BITMAP_BLOCKS + EMPTY_TREE_NODES + FILE_BLOCKS;
	let minimum = ByteLen(occupied * block);
	let exact = |bytes| CapacityPolicy::Exact {
		bytes,
		free_space: ByteLen(0),
	};
	let draft = || plan(blank_options(LARGE_BLOCK), vec![file(PAYLOAD_BYTES, None)]).unwrap();
	let image = draft().seal_with_capacity(exact(minimum)).unwrap();
	assert_eq!(image.image_len(), minimum);
	let primary = header(&image, PRIMARY_HEADER_OFFSET);
	let alternate = header(&image, minimum.0 - ALTERNATE_HEADER_FROM_END);
	assert_eq!(primary, alternate);
	assert_eq!(u64::from(primary.geometry.total_blocks()), occupied);
	assert_eq!(primary.free_blocks, 0);

	let bitmap = primary.forks.allocation.extents().next().unwrap();
	let bitmap_offset = u64::from(bitmap.start.0) * block;
	let mut bits = [0];
	image
		.read_exact_at(ByteOffset(bitmap_offset), &mut bits)
		.unwrap();
	assert_eq!(bits, [u8::MAX << (u64::from(u8::BITS) - occupied)]);

	let short = ByteLen(minimum.0 - block);
	assert!(matches!(
		draft().seal_with_capacity(exact(short)),
		Err(CapacityError::InsufficientSpace { required, available })
			if required == minimum && available == short
	));
}

#[test]
fn crosses_the_bitmap_allocation_block_boundary_exactly() {
	let block_size = u32::try_from(SECTOR).unwrap();
	let empty = plan::<NeverRead>(blank_options(block_size), vec![])
		.unwrap()
		.capacity(auto(0))
		.unwrap();
	let bitmap_capacity = SECTOR * u64::from(u8::BITS);
	let data_blocks = bitmap_capacity - u64::from(empty.geometry().total_blocks());
	let mut draft = plan(
		blank_options(block_size),
		vec![file(data_blocks * SECTOR, None)],
	)
	.unwrap();
	let boundary = draft.capacity(auto(0)).unwrap();
	assert_eq!(
		u64::from(boundary.geometry().total_blocks()),
		bitmap_capacity
	);
	assert_eq!(boundary.free_bytes(), ByteLen(0));

	let expanded = draft.capacity(auto(1)).unwrap();
	assert_eq!(
		u64::from(expanded.geometry().total_blocks()),
		bitmap_capacity + 2
	);
	assert_eq!(expanded.free_bytes(), ByteLen(SECTOR));
	let image = draft.seal_with_capacity(auto(1)).unwrap();
	let primary = header(&image, PRIMARY_HEADER_OFFSET);
	assert_eq!(primary.geometry, expanded.geometry());
	assert_eq!(primary.forks.allocation.total_blocks, 2);
	assert_eq!(primary.free_blocks, 1);
}

#[test]
fn measures_generated_payloads_and_attributes_from_the_draft() {
	let block = u64::from(LARGE_BLOCK);
	let mut draft: VolumeDraft<'_, NeverRead> = plan(
		blank_options(LARGE_BLOCK),
		vec![ManifestEntry {
			path: GENERATED_PATH,
			metadata: EntryMetadata::new(0o644),
			kind: EntryKind::GeneratedFile { resource: None },
		}],
	)
	.unwrap();
	let frozen_id = draft.catalog_id(GENERATED_PATH).unwrap();
	assert!(matches!(
		draft.capacity(auto(0)),
		Err(CapacityError::Volume(VolumeError::MissingGeneratedData(id))) if id == frozen_id
	));
	draft
		.bind_generated_data(
			GENERATED_PATH,
			ForkSource::whole(NeverRead(ByteLen(block + 7))),
		)
		.unwrap();
	let from_forks = draft.capacity(auto(0)).unwrap();
	draft
		.add_inline_attribute(GENERATED_PATH, "com.pomologie.test", b"attribute")
		.unwrap();
	let with_attributes = draft.capacity(auto(0)).unwrap();
	assert!(with_attributes.image_bytes().0 > from_forks.image_bytes().0);
	assert_eq!(draft.catalog_id(GENERATED_PATH), Some(frozen_id));
	let primary = header(
		&draft.seal_with_capacity(auto(0)).unwrap(),
		PRIMARY_HEADER_OFFSET,
	);
	assert_eq!(primary.geometry, with_attributes.geometry());
	assert!(primary.forks.attributes.total_blocks > 0);
}

#[test]
fn charges_bitmap_bytes_in_metadata_and_keeps_large_forks_lazy() {
	let mut options = blank_options(LARGE_BLOCK);
	options.limits.max_metadata_bytes =
		ByteLen(EMPTY_TREE_NODES * NODE_BYTES + (HEADER_BYTES * 2) as u64);
	let mut draft = plan::<NeverRead>(options, vec![]).unwrap();
	assert!(matches!(
		draft.capacity(auto(0)),
		Err(CapacityError::MetadataLimit)
	));

	let payload = 6 * BYTES_PER_GIB + 1;
	let mut draft = plan(blank_options(LARGE_BLOCK), vec![file(payload, None)]).unwrap();
	let selected = draft.capacity(auto(BYTES_PER_GIB)).unwrap();
	assert!(selected.image_bytes().0 > payload + BYTES_PER_GIB);
	let image = draft.seal_with_capacity(auto(BYTES_PER_GIB)).unwrap();
	assert_eq!(image.image_len(), selected.image_bytes());
	assert_eq!(
		u64::from(header(&image, PRIMARY_HEADER_OFFSET).free_blocks) * u64::from(LARGE_BLOCK),
		selected.free_bytes().0
	);
}

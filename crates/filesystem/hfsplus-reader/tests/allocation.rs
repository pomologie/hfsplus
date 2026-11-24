use hfsplus_format::volume::{
	ALTERNATE_HEADER_FROM_END, AllocationBlock, HEADER_BYTES, PRIMARY_HEADER_OFFSET,
};
use hfsplus_reader::{
	SpecialFile, Volume,
	allocation::{AllocationError, AllocationLimits, MetadataOwner},
};

const HEADER: &[u8; HEADER_BYTES] = include_bytes!("fixtures/native-primary.bin");
const BITMAP: &[u8] = include_bytes!("fixtures/native-allocation.bin");
const DEVICE_BYTES: usize = 8 << 20;
const ALLOCATION_BITMAP_AT: usize = 4096;
const NATIVE_TOTAL_BLOCKS: u32 = 2048;
const NATIVE_FREE_BLOCKS: u32 = 1997;
const FIRST_BLOCK_MASK: u8 = 1 << (u8::BITS - 1);
const PRIMARY: usize = PRIMARY_HEADER_OFFSET as usize;
const ALLOCATION_FORK: usize = PRIMARY + 112;
const CATALOG_FORK: usize = PRIMARY + 272;
const FIRST_EXTENT_START: usize = 16;

fn native_volume() -> Vec<u8> {
	let mut raw = vec![0; DEVICE_BYTES];
	raw[PRIMARY..PRIMARY + HEADER_BYTES].copy_from_slice(HEADER);
	let alternate = DEVICE_BYTES - ALTERNATE_HEADER_FROM_END as usize;
	raw[alternate..alternate + HEADER_BYTES].copy_from_slice(HEADER);
	raw[ALLOCATION_BITMAP_AT..ALLOCATION_BITMAP_AT + BITMAP.len()].copy_from_slice(BITMAP);
	raw
}

#[test]
fn reconciles_marked_blocks_on_the_native_empty_volume() {
	let raw = native_volume();
	let volume = Volume::open(raw.as_slice()).unwrap();
	let report = volume
		.check_metadata_allocation(AllocationLimits::default())
		.unwrap();
	assert_eq!(report.total_blocks, NATIVE_TOTAL_BLOCKS);
	assert_eq!(report.free_blocks, NATIVE_FREE_BLOCKS);
	assert_eq!(
		report.metadata_blocks,
		NATIVE_TOTAL_BLOCKS - NATIVE_FREE_BLOCKS
	);
	assert_eq!(report.other_allocated_blocks, 0);
}

#[test]
fn detects_unmarked_metadata_before_accounting_other_blocks() {
	let mut raw = native_volume();
	raw[ALLOCATION_BITMAP_AT] &= !FIRST_BLOCK_MASK;
	let volume = Volume::open(raw.as_slice()).unwrap();
	assert!(matches!(
		volume.check_metadata_allocation(AllocationLimits::default()),
		Err(AllocationError::MarkedFree {
			block: AllocationBlock(0),
			owner: MetadataOwner::PrimaryRegion
		})
	));

	let mut raw = native_volume();
	const FREE_BYTE_INDEX: usize = 20;
	raw[ALLOCATION_BITMAP_AT + FREE_BYTE_INDEX] |= FIRST_BLOCK_MASK;
	let volume = Volume::open(raw.as_slice()).unwrap();
	assert!(matches!(
		volume.check_metadata_allocation(AllocationLimits::default()),
		Err(AllocationError::FreeBlocks { declared: NATIVE_FREE_BLOCKS, counted }) if counted == NATIVE_FREE_BLOCKS - 1
	));
}

#[test]
fn rejects_shared_metadata_blocks_and_short_bitmaps() {
	let mut raw = native_volume();
	let bitmap_start: [u8; size_of::<u32>()] = raw[ALLOCATION_FORK + FIRST_EXTENT_START..]
		[..size_of::<u32>()]
		.try_into()
		.unwrap();
	raw[CATALOG_FORK + FIRST_EXTENT_START..][..size_of::<u32>()].copy_from_slice(&bitmap_start);
	let volume = Volume::open(raw.as_slice()).unwrap();
	assert!(matches!(
		volume.check_metadata_allocation(AllocationLimits::default()),
		Err(AllocationError::Overlap { block, first: MetadataOwner::SpecialFile(first), second: MetadataOwner::SpecialFile(second) })
			if block == AllocationBlock(u32::from_be_bytes(bitmap_start)) && matches!((first, second), (SpecialFile::Allocation, SpecialFile::Catalog) | (SpecialFile::Catalog, SpecialFile::Allocation))
	));

	let mut raw = native_volume();
	let required = BITMAP.len() as u64;
	raw[ALLOCATION_FORK..][..size_of::<u64>()].copy_from_slice(&(required - 1).to_be_bytes());
	let volume = Volume::open(raw.as_slice()).unwrap();
	assert!(matches!(
		volume.check_metadata_allocation(AllocationLimits::default()),
		Err(AllocationError::BitmapLength { required: need, actual }) if need == required && actual == required - 1
	));
}

#[test]
fn checks_bitmap_and_metadata_range_limits() {
	let raw = native_volume();
	let volume = Volume::open(raw.as_slice()).unwrap();
	let limit = BITMAP.len() as u64 - 1;
	let limits = AllocationLimits {
		max_bitmap_bytes: limit,
		..AllocationLimits::default()
	};
	assert!(matches!(
		volume.check_metadata_allocation(limits),
		Err(AllocationError::BitmapLimit { required, limit: reported }) if required == limit + 1 && reported == limit
	));
	let limits = AllocationLimits {
		max_ranges: 2,
		..AllocationLimits::default()
	};
	assert!(matches!(
		volume.check_metadata_allocation(limits),
		Err(AllocationError::RangeLimit)
	));
}

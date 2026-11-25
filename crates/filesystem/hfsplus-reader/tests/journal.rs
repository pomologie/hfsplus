use std::{cell::RefCell, num::NonZeroUsize, ops::Range};

use hfsplus_core::{ByteLen, ByteOffset, ByteRange, ReadAt};
use hfsplus_format::{
	journal::{HEADER_PREFIX_BYTES, INFO_PREFIX_BYTES},
	volume::{
		CLEAN_UNMOUNT_ATTRIBUTE, CatalogId, HEADER_BYTES, INCONSISTENT_ATTRIBUTE,
		JOURNALED_ATTRIBUTE, PRIMARY_HEADER_OFFSET, SPARED_BLOCKS_ATTRIBUTE,
		UNUSED_NODE_FIX_ATTRIBUTE,
	},
};
use hfsplus_reader::{
	AlternateHeader, Error, ForkReadSupport, SpecialFile, Volume,
	catalog::{CatalogError, CatalogLimits, HardLinkLimits},
	check::CheckLimits,
	decmpfs::LogicalDataLimits,
};

const NATIVE: &[u8] = include_bytes!("fixtures/native-journal.bin");
const PAYLOAD_NAME: &str = "message.txt";
const PAYLOAD: &[u8] = b"journal admission fixture\n";
const JOURNAL_IDS: [CatalogId; 2] = [CatalogId(16), CatalogId(17)];
const JOURNAL_NAMES: [&str; 2] = [".journal", ".journal_info_block"];
const PRIMARY: u64 = PRIMARY_HEADER_OFFSET;
const ALTERNATE: u64 = 25_164_800;
const INFO: u64 = 8_192;
const JOURNAL: u64 = 12_288;
const CATALOG: u64 = 2_895_872;
const ATTRIBUTES_OFFSET: u64 = PRIMARY + 4;
const LAST_MOUNT_OFFSET: u64 = PRIMARY + 8;
const INFO_BLOCK_OFFSET: u64 = PRIMARY + 12;
const INFO_FLAGS_OFFSET: u64 = INFO;
const INFO_JOURNAL_OFFSET: u64 = INFO + 36;
const INFO_JOURNAL_SIZE: u64 = INFO + 44;
const CHECKSUM_OFFSET: u64 = JOURNAL + 36;
const JOURNAL_END_OFFSET: u64 = JOURNAL + 16;
const FLAG_EXTERNAL: u32 = 1 << 1;
const FLAG_NEEDS_INIT: u32 = 1 << 2;
const FLAG_UNKNOWN: u32 = 1 << 3;
const NATIVE_FLAGS: u32 = CLEAN_UNMOUNT_ATTRIBUTE | JOURNALED_ATTRIBUTE | UNUSED_NODE_FIX_ATTRIBUTE;
const PENDING_END: u64 = 64_000;
const PENDING_CHECKSUM: u32 = 0x0ab8_a173;
const BIG_ENDIAN_CHECKSUM: u32 = 0x8793_5f75;
const SMALLER_JOURNAL_SIZE: u64 = 520_192;

#[test]
fn clean_native_journal_allows_bounded_catalog_and_file_reads() {
	let source = Fixture::native();
	let volume = Volume::open(&source).unwrap();
	assert_eq!(volume.fork_read_support(), ForkReadSupport::Supported);
	assert!(matches!(
		volume.alternate_header(),
		AlternateHeader::Differs(_)
	));
	assert_eq!(
		*source.reads.borrow(),
		[
			(PRIMARY, HEADER_BYTES),
			(ALTERNATE, HEADER_BYTES),
			(INFO, INFO_PREFIX_BYTES),
			(JOURNAL, HEADER_PREFIX_BYTES),
		],
	);

	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	let root = catalog.root().unwrap().metadata.id;
	let mut cursor = catalog.read_dir(root).unwrap();
	let page = cursor.next_page(NonZeroUsize::new(2).unwrap()).unwrap();
	assert!(page.complete);
	assert_eq!(page.entries.len(), 1);
	assert_eq!(
		page.entries[0].name,
		PAYLOAD_NAME.encode_utf16().collect::<Vec<_>>()
	);
	for (id, name) in JOURNAL_IDS.into_iter().zip(JOURNAL_NAMES) {
		assert!(catalog.entry(id).unwrap().is_none());
		assert!(catalog.lookup(root, name).unwrap().is_none());
	}
	let file = catalog
		.regular_file(page.entries[0].metadata.id, HardLinkLimits::default())
		.unwrap()
		.unwrap();
	let fork = file
		.open_logical_data(LogicalDataLimits::default())
		.unwrap();
	let mut bytes = [0; PAYLOAD.len()];
	fork.read_exact_at(ByteOffset(0), &mut bytes).unwrap();
	assert_eq!(fork.len(), ByteLen(PAYLOAD.len() as u64));
	assert_eq!(bytes, PAYLOAD);

	let report = volume.check_structure(CheckLimits::default()).unwrap();
	assert_eq!(report.catalog_files, 3);
	assert_eq!(report.reachable_fork_blocks, 130);
	assert_eq!(report.unclaimed_allocated_blocks, 0);
}

#[test]
fn pending_headers_and_dirty_primary_each_refuse_logical_reads() {
	let mut source = Fixture::native();
	source.patch(JOURNAL_END_OFFSET, &PENDING_END.to_le_bytes());
	source.patch(CHECKSUM_OFFSET, &PENDING_CHECKSUM.to_le_bytes());
	check_refusal(&source, ForkReadSupport::NeedsJournalReplay, 4);

	for flags in [
		NATIVE_FLAGS & !CLEAN_UNMOUNT_ATTRIBUTE,
		NATIVE_FLAGS | INCONSISTENT_ATTRIBUTE,
	] {
		let mut source = Fixture::native();
		source.patch(ATTRIBUTES_OFFSET, &flags.to_be_bytes());
		check_refusal(&source, ForkReadSupport::NeedsJournalReplay, 2);
	}
}

#[test]
fn admits_the_same_empty_header_in_big_endian_wire_order() {
	let mut source = Fixture::native();
	// Sizes of the eight fields covered by the original header checksum.
	let mut offset = JOURNAL;
	for size in [4, 4, 8, 8, 8, 4, 4, 4] {
		source.bytes_mut(offset, size).reverse();
		offset += size as u64;
	}
	source.patch(CHECKSUM_OFFSET, &BIG_ENDIAN_CHECKSUM.to_be_bytes());
	assert_eq!(
		Volume::open(&source).unwrap().fork_read_support(),
		ForkReadSupport::Supported
	);
}

#[test]
fn unverified_journal_metadata_remains_inspectable_without_logical_reads() {
	let mut corrupted = Fixture::native();
	corrupted.patch(CHECKSUM_OFFSET, &[0; size_of::<u32>()]);
	check_refusal(&corrupted, ForkReadSupport::UnverifiedJournal, 4);

	let mut mismatched = Fixture::native();
	mismatched.patch(INFO_JOURNAL_SIZE, &SMALLER_JOURNAL_SIZE.to_be_bytes());
	check_refusal(&mismatched, ForkReadSupport::UnverifiedJournal, 4);

	for flag in [FLAG_EXTERNAL, FLAG_NEEDS_INIT, FLAG_UNKNOWN] {
		let mut source = Fixture::native();
		let flags = u32::from_be_bytes(
			source
				.bytes_mut(INFO_FLAGS_OFFSET, size_of::<u32>())
				.try_into()
				.unwrap(),
		);
		source.patch(INFO_FLAGS_OFFSET, &(flags | flag).to_be_bytes());
		check_refusal(&source, ForkReadSupport::UnverifiedJournal, 3);
	}
	let mut stale = Fixture::native();
	const UNJOURNALED_MOUNT_VERSION: [u8; 4] = *b"10.0";
	stale.patch(LAST_MOUNT_OFFSET, &UNJOURNALED_MOUNT_VERSION);
	check_refusal(&stale, ForkReadSupport::UnverifiedJournal, 2);

	let mut spared = Fixture::native();
	spared.patch(
		ATTRIBUTES_OFFSET,
		&(NATIVE_FLAGS | SPARED_BLOCKS_ATTRIBUTE).to_be_bytes(),
	);
	check_refusal(&spared, ForkReadSupport::SparedBlocks, 2);
}

#[test]
fn refuses_journal_ranges_outside_data_or_overlapping_metadata_before_reading() {
	let mut source = Fixture::native();
	const BITMAP_BLOCK: u32 = 1;
	source.patch(INFO_BLOCK_OFFSET, &BITMAP_BLOCK.to_be_bytes());
	check_refusal(&source, ForkReadSupport::UnverifiedJournal, 2);

	for offset in [
		0,
		INFO,
		JOURNAL + 1,
		CATALOG,
		ALTERNATE + HEADER_BYTES as u64,
	] {
		let mut source = Fixture::native();
		source.patch(INFO_JOURNAL_OFFSET, &offset.to_be_bytes());
		check_refusal(&source, ForkReadSupport::UnverifiedJournal, 3);
	}
	let mut overflowing = Fixture::native();
	overflowing.patch(INFO_JOURNAL_SIZE, &u64::MAX.to_be_bytes());
	check_refusal(&overflowing, ForkReadSupport::UnverifiedJournal, 3);
}

#[test]
fn journal_source_errors_report_the_failed_byte_offset_and_cause() {
	for offset in [INFO, JOURNAL] {
		let mut source = Fixture::native();
		source.fail_at = Some(offset);
		assert!(matches!(
			Volume::open(&source),
			Err(Error::Source { offset: ByteOffset(actual), error: SourceError::Injected }) if actual == offset
		));
	}
}

fn check_refusal(source: &Fixture, status: ForkReadSupport, expected_reads: usize) {
	let volume = Volume::open(source).unwrap();
	assert_eq!(volume.fork_read_support(), status);
	assert!(volume.header().journal_info.is_some());
	assert!(matches!(
		volume.open_special_fork(SpecialFile::Catalog),
		Err(Error::UnsupportedVolume(actual)) if actual == status
	));
	assert!(matches!(
		volume.open_catalog(CatalogLimits::default()),
		Err(CatalogError::Read(Error::UnsupportedVolume(actual))) if actual == status
	));
	assert_eq!(source.reads.borrow().len(), expected_reads);
}

struct Fixture {
	len: ByteLen,
	spans: Vec<(u64, Range<usize>)>,
	bytes: Vec<u8>,
	reads: RefCell<Vec<(u64, usize)>>,
	fail_at: Option<u64>,
}

impl Fixture {
	fn native() -> Self {
		let mut remaining = NATIVE;
		let len = ByteLen(u64::from_be_bytes(take(&mut remaining)));
		let spans = (0..u16::from_be_bytes(take(&mut remaining)))
			.map(|_| {
				let offset = u64::from_be_bytes(take(&mut remaining));
				let size = u32::from_be_bytes(take(&mut remaining)) as usize;
				let start = NATIVE.len() - remaining.len();
				remaining = &remaining[size..];
				(offset, start..start + size)
			})
			.collect();
		assert!(remaining.is_empty());
		Self {
			len,
			spans,
			bytes: NATIVE.to_vec(),
			reads: RefCell::default(),
			fail_at: None,
		}
	}

	fn patch(&mut self, offset: u64, bytes: &[u8]) {
		self.bytes_mut(offset, bytes.len()).copy_from_slice(bytes);
	}

	fn bytes_mut(&mut self, offset: u64, len: usize) -> &mut [u8] {
		let span = self.span(offset, len).unwrap();
		&mut self.bytes[span]
	}

	fn span(&self, offset: u64, len: usize) -> Option<Range<usize>> {
		self.spans.iter().find_map(|(start, span)| {
			let relative = usize::try_from(offset.checked_sub(*start)?).ok()?;
			if relative.checked_add(len)? > span.len() {
				return None;
			}
			let start = span.start + relative;
			Some(start..start + len)
		})
	}
}

impl ReadAt for Fixture {
	type Error = SourceError;

	fn len(&self) -> ByteLen {
		self.len
	}

	fn read_exact_at(&self, offset: ByteOffset, destination: &mut [u8]) -> Result<(), Self::Error> {
		ByteRange::new(offset, destination.len().into(), self.len)
			.map_err(|_| SourceError::Uncaptured)?;
		self.reads.borrow_mut().push((offset.0, destination.len()));
		if self.fail_at == Some(offset.0) {
			return Err(SourceError::Injected);
		}
		let span = self
			.span(offset.0, destination.len())
			.ok_or(SourceError::Uncaptured)?;
		destination.copy_from_slice(&self.bytes[span]);
		Ok(())
	}
}

#[derive(Debug)]
enum SourceError {
	Uncaptured,
	Injected,
}

fn take<const N: usize>(bytes: &mut &[u8]) -> [u8; N] {
	let (field, rest) = bytes.split_at(N);
	*bytes = rest;
	field.try_into().unwrap()
}

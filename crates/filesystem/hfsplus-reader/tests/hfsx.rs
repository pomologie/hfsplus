//! Native and native-derived catalog fixtures for both HFSX modes in H-READ-02.

use std::num::NonZeroUsize;

use hfsplus_core::{ByteLen, ByteOffset, ReadAt, names::NameCollation};
use hfsplus_format::{btree::CASE_FOLDING_COMPARE, volume::Signature};
use hfsplus_reader::{
	ForkReadSupport, Volume,
	catalog::{CatalogError, CatalogLimits, Corruption, HardLinkLimits, ROOT_ID},
	check::{CheckError, CheckLimits},
	decmpfs::LogicalDataLimits,
};

#[path = "hfsx/case_folded.rs"]
mod case_folded;

const PLAIN: &[u8] = include_bytes!("fixtures/native-hfsx.bin");
const JOURNALED: &[u8] = include_bytes!("fixtures/native-hfsx-journaled.bin");
const LABEL: &str = "HFSX Fixture";
const FOLDER: &str = "Cases";
const LOWER_FOLDER: &str = "cases";
const EMPTY_COUNT: usize = 72;
const PAGE_SIZE: NonZeroUsize = NonZeroUsize::new(13).unwrap();
const BINARY_CATALOG_OFFSET: usize = 2_367_488;
const CATALOG_KEY_COMPARE_OFFSET: usize = 51;
const UNKNOWN_COMPARE: u8 = 0xff;
const PAYLOADS: &[(&str, &[u8])] = &[
	("Case.txt", b"capital\0\x01\xff\n"),
	("case.txt", b"lowercase\0\x02\xfe\n"),
	("Å.txt", b"upper ring\n"),
	("å.txt", b"lower ring\n"),
	("join.txt", b"plain\n"),
	("jo\u{200d}in.txt", b"joiner significant\n"),
	("\u{200c}", b"sole non-joiner\n"),
	("😀.txt", b"emoji\n"),
];

#[test]
fn opens_both_clean_native_profiles_with_binary_lookup_and_exact_payloads() {
	for (fixture, journaled) in [(PLAIN, false), (JOURNALED, true)] {
		let image = expand(fixture);
		let volume = Volume::open(image.as_slice()).unwrap();
		assert_eq!(volume.header().signature, Signature::HfsX);
		assert_eq!(volume.header().journal_info.is_some(), journaled);
		assert_eq!(volume.fork_read_support(), ForkReadSupport::Supported);

		let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
		assert_eq!(catalog.collation(), NameCollation::Binary);
		assert!(
			catalog
				.root()
				.unwrap()
				.name
				.into_iter()
				.eq(LABEL.encode_utf16())
		);
		let directory = catalog
			.lookup(ROOT_ID, FOLDER)
			.unwrap()
			.unwrap()
			.metadata
			.id;
		let lower_directory = catalog
			.lookup(ROOT_ID, LOWER_FOLDER)
			.unwrap()
			.unwrap()
			.metadata
			.id;
		assert_ne!(directory, lower_directory);
		assert!(catalog.lookup(ROOT_ID, "CASES").unwrap().is_none());
		assert!(catalog.lookup(directory, "CASE.txt").unwrap().is_none());
		assert!(catalog.lookup(directory, "JOIN.txt").unwrap().is_none());
		assert!(catalog.lookup(ROOT_ID, ".journal").unwrap().is_none());
		assert!(catalog.lookup_raw(directory, &[]).is_err());

		for &(name, wanted) in PAYLOADS {
			let entry = catalog.lookup(directory, name).unwrap().unwrap();
			assert_eq!(
				catalog.lookup_raw(directory, &entry.name).unwrap(),
				Some(entry.clone())
			);
			assert_eq!(
				catalog.entry(entry.metadata.id).unwrap(),
				Some(entry.clone())
			);
			let file = catalog
				.regular_file(entry.metadata.id, HardLinkLimits::default())
				.unwrap()
				.unwrap();
			let data = file
				.open_logical_data(LogicalDataLimits::default())
				.unwrap();
			assert_eq!(data.len(), ByteLen(wanted.len() as u64));
			let mut read = vec![0; wanted.len()];
			data.read_exact_at(ByteOffset(0), &mut read).unwrap();
			assert_eq!(read, wanted, "{name}");
		}
	}
}

#[test]
fn enumerates_and_audits_native_binary_order_across_leaves() {
	let mut expected: Vec<String> = [
		"A\u{30a}.txt",
		"Case.txt",
		"a\u{30a}.txt",
		"case.txt",
		"join.txt",
		"jo\u{200d}in.txt",
	]
	.into_iter()
	.map(String::from)
	.collect();
	expected.extend((0..EMPTY_COUNT).map(|index| format!("middle-{index:03}.txt")));
	expected.extend(["\u{200c}".into(), "😀.txt".into()]);

	for (fixture, journaled) in [(PLAIN, false), (JOURNALED, true)] {
		let image = expand(fixture);
		let volume = Volume::open(image.as_slice()).unwrap();
		let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
		let root = catalog
			.read_dir(ROOT_ID)
			.unwrap()
			.next_page(PAGE_SIZE)
			.unwrap();
		assert!(root.complete);
		assert_eq!(root.entries.len(), 2);
		for (entry, name) in root.entries.iter().zip([FOLDER, LOWER_FOLDER]) {
			assert!(entry.name.iter().copied().eq(name.encode_utf16()));
		}
		let mut cursor = catalog.read_dir(root.entries[0].metadata.id).unwrap();
		let mut names = Vec::new();
		loop {
			let page = cursor.next_page(PAGE_SIZE).unwrap();
			names.extend(
				page.entries
					.into_iter()
					.map(|entry| String::from_utf16(&entry.name).unwrap()),
			);
			if page.complete {
				break;
			}
		}
		assert_eq!(names, expected);

		let expected_files = (EMPTY_COUNT + PAYLOADS.len()) as u32 + if journaled { 2 } else { 0 };
		let report = volume.check_structure(CheckLimits::default()).unwrap();
		assert_eq!(report.catalog_files, expected_files);
		assert_eq!(report.catalog_folders, 4);
		assert_eq!(report.unclaimed_allocated_blocks, 0);
	}
}

#[test]
fn reports_an_unqualified_hfsx_comparison_type() {
	let mut image = expand(PLAIN);
	image[BINARY_CATALOG_OFFSET + CATALOG_KEY_COMPARE_OFFSET] = UNKNOWN_COMPARE;
	let volume = Volume::open(image.as_slice()).unwrap();
	assert_eq!(volume.header().signature, Signature::HfsX);
	assert!(matches!(
		volume.open_catalog(CatalogLimits::default()),
		Err(CatalogError::UnsupportedComparison {
			key_compare_type: UNKNOWN_COMPARE
		})
	));
}

#[test]
fn rejects_a_binary_catalog_falsely_declaring_case_folding() {
	let mut image = expand(PLAIN);
	image[BINARY_CATALOG_OFFSET + CATALOG_KEY_COMPARE_OFFSET] = CASE_FOLDING_COMPARE;
	let volume = Volume::open(image.as_slice()).unwrap();
	assert!(matches!(
		volume.check_structure(CheckLimits::default()),
		Err(CheckError::Catalog(CatalogError::Corrupt {
			reason: Corruption::RecordOrder,
			..
		}))
	));
}

fn expand(mut data: &[u8]) -> Vec<u8> {
	let volume_bytes = u64::from_be_bytes(take(&mut data));
	let runs = u16::from_be_bytes(take(&mut data));
	let mut image = vec![0; usize::try_from(volume_bytes).unwrap()];
	let mut end = 0;
	for _ in 0..runs {
		let offset = usize::try_from(u64::from_be_bytes(take(&mut data))).unwrap();
		let bytes = u32::from_be_bytes(take(&mut data)) as usize;
		assert!(offset >= end && bytes != 0);
		end = offset + bytes;
		let (run, tail) = data.split_at(bytes);
		image[offset..end].copy_from_slice(run);
		data = tail;
	}
	assert!(data.is_empty());
	image
}

fn take<const N: usize>(data: &mut &[u8]) -> [u8; N] {
	let (field, tail) = data.split_at(N);
	*data = tail;
	field.try_into().unwrap()
}

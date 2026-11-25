use hfsplus_core::{ByteLen, ByteOffset, ReadAt, names::NameCollation};
use hfsplus_format::volume::{ALTERNATE_HEADER_FROM_END, PRIMARY_HEADER_OFFSET, Signature};
use hfsplus_reader::{
	AlternateHeader, ForkReadSupport, Volume,
	catalog::{CatalogLimits, EntryKind, HardLinkLimits, ROOT_ID},
	check::{AlternateStatus, CheckLimits},
	decmpfs::LogicalDataLimits,
};

use super::{CASE_FOLDING_COMPARE, CATALOG_KEY_COMPARE_OFFSET, PAGE_SIZE};

const NATIVE: &[u8] = include_bytes!("../fixtures/native-catalog-volume.bin");
const HFS_PLUS_HEADER_ID: [u8; 4] = *b"H+\0\x04";
const HFSX_HEADER_ID: [u8; 4] = *b"HX\0\x05";
const CATALOG_OFFSET: usize = 2_048;
const ROOT_FOLDER_RECORD: usize = 6_200;
const NESTED_FOLDER_RECORD: usize = 32_114;
const FOLDER_FLAGS_OFFSET: usize = 2;
const FOLDER_COUNT_OFFSET: usize = 84;
const HAS_FOLDER_COUNT: u16 = 0x0010;
const DIRECT_FOLDER_COUNT: u32 = 1;
const EMPTY_FILE_COUNT: usize = 96;
const NESTED_NAME: &str = "Nested";
const STORED_FILE_NAME: &str = "Re\u{0301}sume\u{0301}.txt";
const PAYLOAD: &[u8] = b"catalog unicode payload\n";

#[test]
fn folds_lookup_but_preserves_stored_names_folder_metadata_and_payload() {
	let image = qualified_fixture();
	let volume = Volume::open(image.as_slice()).unwrap();
	assert_eq!(volume.header().signature, Signature::HfsX);
	assert_eq!(volume.alternate_header(), AlternateHeader::MatchesPrimary);
	assert_eq!(volume.fork_read_support(), ForkReadSupport::Supported);

	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	assert_eq!(catalog.collation(), NameCollation::CaseFolded);
	let root = catalog.root().unwrap();
	assert_eq!(root.metadata.id, ROOT_ID);
	assert_eq!(root.metadata.flags, HAS_FOLDER_COUNT);
	assert!(matches!(
		root.kind,
		EntryKind::Directory {
			tail_word: DIRECT_FOLDER_COUNT,
			..
		}
	));
	let nested = catalog.lookup(ROOT_ID, "nEsTeD").unwrap().unwrap();
	assert!(nested.name.into_iter().eq(NESTED_NAME.encode_utf16()));
	assert_eq!(nested.metadata.flags, HAS_FOLDER_COUNT);
	assert_eq!(
		nested.kind,
		EntryKind::Directory {
			valence: 1,
			tail_word: 0,
		}
	);
	let folder = nested.metadata.id;
	let file = catalog.lookup(folder, "rÉSUMÉ.TXT").unwrap().unwrap();
	assert!(
		file.name
			.iter()
			.copied()
			.eq(STORED_FILE_NAME.encode_utf16())
	);
	assert_eq!(
		catalog.lookup_raw(folder, &file.name).unwrap(),
		Some(file.clone())
	);
	assert_eq!(catalog.entry(file.metadata.id).unwrap(), Some(file.clone()));
	assert_eq!(catalog.lookup(folder, "resume.txt").unwrap(), None);
	let stored = catalog.lookup(ROOT_ID, "item-095").unwrap();
	assert!(stored.is_some());
	assert_eq!(catalog.lookup(ROOT_ID, "Ite\u{200d}m-095").unwrap(), stored);

	let file = catalog
		.regular_file(file.metadata.id, HardLinkLimits::default())
		.unwrap()
		.unwrap();
	let data = file
		.open_logical_data(LogicalDataLimits::default())
		.unwrap();
	assert_eq!(data.len(), ByteLen(PAYLOAD.len() as u64));
	let mut bytes = [0; PAYLOAD.len()];
	data.read_exact_at(ByteOffset(0), &mut bytes).unwrap();
	assert_eq!(&bytes, PAYLOAD);
}

#[test]
fn paginates_the_native_case_folded_index_and_reconciles_allocation() {
	let image = qualified_fixture();
	let volume = Volume::open(image.as_slice()).unwrap();
	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	let mut cursor = catalog.read_dir(ROOT_ID).unwrap();
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
	let mut expected: Vec<_> = (0..EMPTY_FILE_COUNT)
		.map(|index| format!("item-{index:03}"))
		.collect();
	expected.push(NESTED_NAME.into());
	assert_eq!(names, expected);

	let report = volume.check_structure(CheckLimits::default()).unwrap();
	assert_eq!(report.catalog_files, EMPTY_FILE_COUNT as u32 + 1);
	assert_eq!(report.catalog_folders, DIRECT_FOLDER_COUNT);
	assert_eq!(report.unclaimed_allocated_blocks, 0);
	assert_eq!(report.alternate, AlternateStatus::MatchesPrimary);
}

fn qualified_fixture() -> Vec<u8> {
	// The base is the native optical fixture. Apply the exact field overlay
	// qualified by Apple's read-only checker and mount (see fixture provenance).
	let mut image = NATIVE.to_vec();
	let alternate = image.len() - ALTERNATE_HEADER_FROM_END as usize;
	for offset in [PRIMARY_HEADER_OFFSET as usize, alternate] {
		let field = &mut image[offset..offset + HFSX_HEADER_ID.len()];
		assert_eq!(field, &HFS_PLUS_HEADER_ID);
		field.copy_from_slice(&HFSX_HEADER_ID);
	}
	image[CATALOG_OFFSET + CATALOG_KEY_COMPARE_OFFSET] = CASE_FOLDING_COMPARE;
	for folder in [ROOT_FOLDER_RECORD, NESTED_FOLDER_RECORD] {
		let offset = folder + FOLDER_FLAGS_OFFSET;
		let flags = &mut image[offset..offset + size_of::<u16>()];
		assert_eq!(flags, &[0; size_of::<u16>()]);
		flags.copy_from_slice(&HAS_FOLDER_COUNT.to_be_bytes());
	}
	let offset = ROOT_FOLDER_RECORD + FOLDER_COUNT_OFFSET;
	image[offset..offset + size_of::<u32>()].copy_from_slice(&DIRECT_FOLDER_COUNT.to_be_bytes());
	image
}

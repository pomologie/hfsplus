use hfsplus_builder::volume::{
	EntryKind, EntryMetadata, ForkSource, ManifestEntry, VolumeError, VolumeOptions, plan,
};
use hfsplus_core::names::{MAX_NAME_UNITS, NameCollation, NameError};
use hfsplus_format::{
	btree::{Node, TreeHeader},
	catalog::{Record, ThreadKind, Value},
	volume::{CatalogId, Geometry, Header, PRIMARY_HEADER_OFFSET},
};
use hfsplus_reader::{
	Volume,
	catalog::{CatalogLimits, NameNormalization, ROOT_ID},
};

const ALLOCATION_BLOCK_BYTES: u32 = 4096;
const VOLUME_BLOCKS: u32 = 2048;
const VOLUME_BYTES: u64 = ALLOCATION_BLOCK_BYTES as u64 * VOLUME_BLOCKS as u64;
const LABEL: &str = "Étude 容量";
const PARENT: &str = "Café";
const GENERATED: &str = "CAFÉ/Ébauche";
const IGNORABLE_DISPLAY: &str = "Æ\u{200d}ne";
const REGULAR_MODE: u16 = 0o644;
const DIRECTORY_MODE: u16 = 0o755;
const COMPOSED_MUSIC: &str = "\u{1d15e}";
const SCALAR_STORED_MUSIC: &str = "\u{1d157}\u{1d165}";

fn geometry() -> Geometry {
	Geometry::new(ALLOCATION_BLOCK_BYTES, VOLUME_BLOCKS, VOLUME_BYTES).unwrap()
}

fn folder(path: &str) -> ManifestEntry<'_, &'static [u8]> {
	ManifestEntry {
		path,
		metadata: EntryMetadata::new(DIRECTORY_MODE),
		kind: EntryKind::Folder,
	}
}

fn file(path: &str) -> ManifestEntry<'_, &'static [u8]> {
	ManifestEntry {
		path,
		metadata: EntryMetadata::new(REGULAR_MODE),
		kind: EntryKind::File {
			data: ForkSource::whole(b"file data".as_slice()),
			resource: None,
		},
	}
}

fn build(reverse: bool) -> (Vec<u8>, CatalogId, CatalogId) {
	let mut entries = vec![
		ManifestEntry {
			path: GENERATED,
			metadata: EntryMetadata::new(REGULAR_MODE),
			kind: EntryKind::GeneratedFile { resource: None },
		},
		file("cafe\u{301}/Æ\u{200d}ne"),
		file("CAFÉ/["),
		file("CAFÉ/Σ"),
		file("CAFÉ/ς"),
		file("CAFÉ/😀"),
		folder(PARENT),
	];
	if reverse {
		entries.reverse();
	}
	let mut draft = plan(VolumeOptions::new(LABEL, geometry()), entries).unwrap();
	assert_ne!(
		draft.catalog_id("CAFÉ/σ"),
		draft.catalog_id("CAFÉ/ς"),
		"Greek final sigma remains distinct under the historical comparator"
	);
	let file_id = draft.try_catalog_id("CAFE\u{301}/æNE").unwrap().unwrap();
	let parent_id = draft.try_catalog_id("cafe\u{301}").unwrap().unwrap();
	assert_eq!(draft.try_catalog_id("café/missing").unwrap(), None);
	assert!(matches!(
		draft.try_catalog_id("café//name"),
		Err(VolumeError::InvalidPath)
	));
	let generated_id = draft.catalog_id(GENERATED).unwrap();
	draft
		.bind_generated_data(
			"cafe\u{301}/e\u{301}BAUCHE",
			ForkSource::whole(b"generated data".as_slice()),
		)
		.unwrap();
	assert_eq!(draft.catalog_id(GENERATED), Some(generated_id));
	let mut output = Vec::new();
	draft.seal().unwrap().write_to(&mut output).unwrap();
	(output, parent_id, file_id)
}

#[test]
fn preserves_display_units_and_catalog_order_with_stable_ids() {
	let (image, parent_id, file_id) = build(false);
	let (reordered, reordered_parent, reordered_file) = build(true);
	assert_eq!((reordered_parent, reordered_file), (parent_id, file_id));
	assert_eq!(reordered, image);
	let header = Header::decode(&image[PRIMARY_HEADER_OFFSET as usize..], VOLUME_BYTES).unwrap();
	assert_eq!((header.files, header.folders), (6, 1));
	let catalog = header.forks.catalog;
	let first_extent = catalog.extents().next().unwrap();
	let start = geometry()
		.block_range(first_extent.start, first_extent.blocks)
		.unwrap()
		.start as usize;
	let bytes = &image[start..start + catalog.logical_bytes as usize];
	let node_size = usize::from(TreeHeader::node_size_hint(bytes).unwrap());
	let tree = TreeHeader::decode(
		&Node::decode(&bytes[..node_size]).unwrap(),
		catalog.logical_bytes,
	)
	.unwrap();
	assert_eq!(tree.first_leaf, tree.last_leaf);
	let leaf = tree.first_leaf.unwrap().0 as usize * node_size;
	let node = Node::decode(&bytes[leaf..leaf + node_size]).unwrap();
	let records: Vec<_> = node
		.records()
		.map(|bytes| Record::decode(bytes, geometry()).unwrap())
		.collect();
	let volume_name = records.first().unwrap().key.name;
	assert!(volume_name.units().eq("E\u{301}tude 容量".encode_utf16()));
	let direct_children: Vec<_> = records
		.iter()
		.filter(|record| record.key.parent_or_thread == parent_id && !record.key.name.is_empty())
		.map(|record| String::from_utf16(&record.key.name.units().collect::<Vec<_>>()).unwrap())
		.collect();
	assert_eq!(
		direct_children,
		["[", "E\u{301}bauche", IGNORABLE_DISPLAY, "ς", "Σ", "😀"]
	);
	for pair in records.windows(2) {
		let left = pair[0].key;
		let right = pair[1].key;
		assert!(
			left.parent_or_thread
				.cmp(&right.parent_or_thread)
				.then_with(|| {
					let left = left.name.units().collect::<Vec<_>>();
					let right = right.name.units().collect::<Vec<_>>();
					NameCollation::CaseFolded.compare(&left, &right)
				})
				.is_lt()
		);
	}
	let thread = records
		.iter()
		.find(|record| record.key.parent_or_thread == file_id)
		.unwrap();
	assert!(matches!(
		thread.value,
		Value::Thread { kind: ThreadKind::File, parent, name }
			if parent == parent_id && name.units().eq(IGNORABLE_DISPLAY.encode_utf16())
	));
}

#[test]
fn reports_both_colliding_paths_before_emission() {
	let cases = [
		("Café", "CAFE\u{301}"),
		("Résumé", "Ré\u{200d}sumé"),
		("Café/Σ", "cafe\u{301}/σ"),
		("Æ", "æ"),
	];
	for (first, second) in cases {
		let mut expected = [first, second];
		expected.sort();
		let mut entries = vec![file(first), file(second)];
		if first.contains('/') {
			entries.push(folder(PARENT));
		}
		assert!(matches!(
			plan(VolumeOptions::new(LABEL, geometry()), entries),
			Err(VolumeError::PathCollision { first, second })
				if [first.as_str(), second.as_str()] == expected
		));
	}
}

#[test]
fn rejects_the_historical_georgian_collision() {
	const CAPITAL: &str = "\u{10a0}";
	const HISTORICAL_SMALL: &str = "\u{10d0}";
	const MODERN_SMALL: &str = "\u{2d00}";
	let options = || VolumeOptions::new(LABEL, geometry());
	for [left, right] in [[CAPITAL, HISTORICAL_SMALL], [HISTORICAL_SMALL, CAPITAL]] {
		assert!(matches!(
			plan(options(), vec![file(left), file(right)]),
			Err(VolumeError::PathCollision { first, second })
				if [first.as_str(), second.as_str()] == [CAPITAL, HISTORICAL_SMALL]
		));
	}
	let draft = plan(options(), vec![file(CAPITAL), file(MODERN_SMALL)]).unwrap();
	assert_ne!(
		draft.catalog_id(CAPITAL).unwrap(),
		draft.catalog_id(MODERN_SMALL).unwrap()
	);
}

#[test]
fn rejects_unaddressable_components_and_private_names() {
	let options = || VolumeOptions::new(LABEL, geometry());
	for path in [
		"/absolute",
		"x/",
		"x//y",
		".\u{200d}",
		"\u{200c}..",
		"\u{feff}",
	] {
		assert!(matches!(
			plan(options(), vec![file(path)]),
			Err(VolumeError::InvalidPath)
		));
	}
	for path in [
		"a:b",
		"raw\0null",
		"null-symbol-\u{2400}",
		"noncharacter-\u{fffe}",
		"noncharacter-\u{ffff}",
		"\u{ffff}\u{ffff}\u{ffff}\u{ffff}HFS+ Private Data",
		"␀␀␀␀hfs+ private data",
	] {
		assert!(matches!(
			plan(options(), vec![file(path)]),
			Err(VolumeError::UnsupportedName)
		));
	}
	assert!(matches!(
		plan(
			options(),
			vec![folder(".hfs+ PRIVATE\u{200d} Directory Data\r")]
		),
		Err(VolumeError::ReservedPath)
	));
	assert!(matches!(
		plan::<&[u8]>(VolumeOptions::new("\u{200d}", geometry()), Vec::new()),
		Err(VolumeError::UnsupportedName)
	));
}

#[test]
fn enforces_the_limit_after_decomposition_and_utf16_encoding() {
	let options = || VolumeOptions::new(LABEL, geometry());
	let max_supplementary = "😀".repeat(MAX_NAME_UNITS / 2) + "界";
	let max_label = "界".repeat(MAX_NAME_UNITS);
	for name in [
		"a".repeat(MAX_NAME_UNITS),
		max_supplementary,
		"é".repeat(MAX_NAME_UNITS / 2) + "a",
	] {
		plan(
			VolumeOptions::new(&max_label, geometry()),
			vec![file(&name)],
		)
		.unwrap()
		.seal()
		.unwrap();
	}
	let beyond_after_decomposition = "é".repeat(MAX_NAME_UNITS.div_ceil(2));
	for name in [
		"a".repeat(MAX_NAME_UNITS + 1),
		"😀".repeat(MAX_NAME_UNITS.div_ceil(2)),
		beyond_after_decomposition.clone(),
	] {
		assert!(matches!(
			plan(options(), vec![file(&name)]),
			Err(VolumeError::Normalization(NameError::TooLong))
		));
	}
	assert!(matches!(
		plan::<&[u8]>(
			VolumeOptions::new(&beyond_after_decomposition, geometry()),
			Vec::new()
		),
		Err(VolumeError::Normalization(NameError::TooLong))
	));
}

#[test]
fn preserves_native_supplementary_siblings_and_explicit_optical_lookup() {
	let entries = vec![file(COMPOSED_MUSIC), file(SCALAR_STORED_MUSIC)];
	let plan = plan(VolumeOptions::new(LABEL, geometry()), entries)
		.unwrap()
		.seal()
		.unwrap();
	let mut image = Vec::new();
	plan.write_to(&mut image).unwrap();
	let volume = Volume::open(image.as_slice()).unwrap();
	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	let composed = catalog.lookup(ROOT_ID, COMPOSED_MUSIC).unwrap().unwrap();
	let scalar = catalog
		.lookup(ROOT_ID, SCALAR_STORED_MUSIC)
		.unwrap()
		.unwrap();
	assert_ne!(composed.metadata.id, scalar.metadata.id);
	assert!(
		composed
			.name
			.iter()
			.copied()
			.eq(COMPOSED_MUSIC.encode_utf16())
	);
	assert!(
		scalar
			.name
			.iter()
			.copied()
			.eq(SCALAR_STORED_MUSIC.encode_utf16())
	);
	assert_eq!(
		catalog.lookup_raw(ROOT_ID, &composed.name).unwrap(),
		Some(composed)
	);
	assert_eq!(
		catalog
			.lookup_with_normalization(ROOT_ID, COMPOSED_MUSIC, NameNormalization::Unicode32Scalars)
			.unwrap(),
		Some(scalar)
	);
}

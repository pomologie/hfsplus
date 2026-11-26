//! Raw catalog timestamp boundaries for H-FMT-04-T02.

use std::{env, fs::File};

use hfsplus_builder::{
	SealedPlan,
	volume::{EntryKind, EntryMetadata, ForkSource, ManifestEntry, VolumeOptions, plan},
};
use hfsplus_format::{
	date::{MISSING, from_unix_seconds},
	volume::Geometry,
};
use hfsplus_reader::Volume;

const LABEL: &str = "Catalog Dates";
const BLOCK_BYTES: u32 = 4096;
const IMAGE_BYTES: u64 = 8 * 1024 * 1024;
const FIRST_UNIX_SECONDS: i64 = -2_082_844_799;
const LAST_UNIX_SECONDS: i64 = 2_212_122_495;
const UNIX_EPOCH_HFS_SECONDS: u32 = 2_082_844_800;
const DATE_FIELD_COUNT: usize = 5;
const OUTPUT_ENV: &str = "POMOLOGIE_CATALOG_DATES_OUTPUT";
const CASES: [(&str, Option<i64>, u32); 4] = [
	("Missing", None, MISSING),
	("Earliest", Some(FIRST_UNIX_SECONDS), 1),
	("Unix-Epoch", Some(0), UNIX_EPOCH_HFS_SECONDS),
	("Last", Some(LAST_UNIX_SECONDS), u32::MAX),
];

fn fixture() -> SealedPlan<&'static [u8]> {
	let geometry = Geometry::new(
		BLOCK_BYTES,
		(IMAGE_BYTES / u64::from(BLOCK_BYTES)) as u32,
		IMAGE_BYTES,
	)
	.unwrap();
	let entries = CASES
		.into_iter()
		.map(|(path, seconds, _)| {
			let date = seconds.map_or(MISSING, |seconds| from_unix_seconds(seconds).unwrap().get());
			ManifestEntry {
				path,
				metadata: EntryMetadata {
					dates: [date; DATE_FIELD_COUNT],
					..EntryMetadata::new(0o644)
				},
				kind: EntryKind::File {
					data: ForkSource::whole(&[][..]),
					resource: None,
				},
			}
		})
		.collect();
	plan(VolumeOptions::new(LABEL, geometry), entries)
		.unwrap()
		.seal()
		.unwrap()
}

#[test]
fn preserves_raw_dates_and_refuses_unrepresentable_unix_seconds() {
	let image = fixture();
	let volume = Volume::open(&image).unwrap();
	let catalog = volume.open_catalog(Default::default()).unwrap();
	let root = catalog.root().unwrap().metadata.id;
	for (name, _, date) in CASES {
		let entry = catalog.lookup(root, name).unwrap().unwrap();
		assert_eq!(entry.metadata.dates, [date; DATE_FIELD_COUNT]);
	}
	assert_eq!(from_unix_seconds(FIRST_UNIX_SECONDS - 1), None);
	assert_eq!(from_unix_seconds(LAST_UNIX_SECONDS + 1), None);
}

#[test]
#[ignore = "writes a disposable timestamp witness for native read-only stat"]
fn emit_metadata_dates() {
	let output = File::create_new(env::var_os(OUTPUT_ENV).unwrap()).unwrap();
	fixture().write_to(output).unwrap();
}

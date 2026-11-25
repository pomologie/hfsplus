//! Synthetic raw HFS+ witness for H-BUILD-05-T01 and T02.

use std::{env, fs::File};

use hfsplus_builder::{
	SealedPlan,
	volume::{
		EntryKind, EntryMetadata, ForkSource, HardLinkGroup, ManifestEntry, VolumeOptions,
		plan_with_hard_links,
	},
};
use hfsplus_format::volume::Geometry;

const VOLUME_LABEL: &str = "Pomologie Fixture";
const BLOCK_BYTES: u32 = 4096;
const BLOCK_COUNT: u32 = 2048;
const IMAGE_BYTES: u64 = BLOCK_BYTES as u64 * BLOCK_COUNT as u64;
const HFS_DATE: u32 = 3_787_040_096;
const OWNER: u32 = 1234;
const GROUP: u32 = 567;
const DIRECTORY_MODE: u16 = 0o755;
const FILE_MODE: u16 = 0o644;
const EXECUTABLE_MODE: u16 = 0o751;
const PRIMARY: &str = "Synthetic.app/Contents/MacOS/Witness";
const ALIAS: &str = "Synthetic.app/Contents/Resources/WitnessAlias";
const LINK: &str = "Synthetic.app/Contents/Resources/Start";
const LINK_TARGET: &[u8] = b"../MacOS/Witness";
const GUIDE: &str = "Synthetic.app/Contents/Resources/Café.txt";
const GUIDE_DATA: &[u8] = b"Unicode HFS Plus witness\n";
const INFO: &str = "Synthetic.app/Contents/Info.plist";
const INFO_DATA: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>Witness</string>
<key>CFBundleIdentifier</key><string>org.pomologie.hfsplus.witness</string>
<key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>"#;
const EXECUTABLE_DATA: &[u8] = b"#!/bin/sh\nexit 0\n";
const RESOURCE_DATA: &[u8] = b"Pomologie\0resource fork\n";
const LINK_ATTRIBUTE: &str = "org.pomologie.witness.shared";
const LINK_ATTRIBUTE_VALUE: &[u8] = b"shared\0attribute\xff";
const GUIDE_ATTRIBUTE: &str = "org.pomologie.witness.guide";
const GUIDE_ATTRIBUTE_VALUE: &[u8] = b"guide metadata";
const OUTPUT_ENV: &str = "POMOLOGIE_RAW_HFS_WITNESS_OUTPUT";
const REVERSE_ENV: &str = "POMOLOGIE_RAW_HFS_WITNESS_REVERSE";

fn metadata(mode: u16) -> EntryMetadata {
	EntryMetadata {
		uid: OWNER,
		gid: GROUP,
		dates: [HFS_DATE; 5],
		..EntryMetadata::new(mode)
	}
}

fn regular(path: &'static str, data: &'static [u8]) -> ManifestEntry<'static, &'static [u8]> {
	ManifestEntry {
		path,
		metadata: metadata(FILE_MODE),
		kind: EntryKind::File {
			data: ForkSource::whole(data),
			resource: None,
		},
	}
}

fn canonical_volume(reverse: bool) -> SealedPlan<&'static [u8]> {
	let geometry = Geometry::new(BLOCK_BYTES, BLOCK_COUNT, IMAGE_BYTES).unwrap();
	let mut options = VolumeOptions::new(VOLUME_LABEL, geometry);
	options.root = metadata(DIRECTORY_MODE);
	options.dates = [HFS_DATE; 4];
	let mut entries: Vec<_> = [
		"Synthetic.app",
		"Synthetic.app/Contents",
		"Synthetic.app/Contents/MacOS",
		"Synthetic.app/Contents/Resources",
	]
	.into_iter()
	.map(|path| ManifestEntry {
		path,
		metadata: metadata(DIRECTORY_MODE),
		kind: EntryKind::Folder,
	})
	.collect();
	entries.extend([
		regular(INFO, INFO_DATA),
		regular(GUIDE, GUIDE_DATA),
		ManifestEntry {
			path: LINK,
			metadata: metadata(EXECUTABLE_MODE),
			kind: EntryKind::Symlink {
				target: LINK_TARGET.into(),
			},
		},
	]);
	let mut group = HardLinkGroup {
		paths: vec![PRIMARY, ALIAS],
		metadata: metadata(EXECUTABLE_MODE),
		data: ForkSource::whole(EXECUTABLE_DATA),
		resource: Some(ForkSource::whole(RESOURCE_DATA)),
	};
	let mut attributes = [
		(PRIMARY, LINK_ATTRIBUTE, LINK_ATTRIBUTE_VALUE),
		(GUIDE, GUIDE_ATTRIBUTE, GUIDE_ATTRIBUTE_VALUE),
	];
	if reverse {
		entries.reverse();
		group.paths.reverse();
		attributes.reverse();
	}
	let mut draft = plan_with_hard_links(options, entries, vec![group]).unwrap();
	for (path, name, data) in attributes {
		draft.add_inline_attribute(path, name, data).unwrap();
	}
	draft.seal().unwrap()
}

#[test]
fn canonical_volume_survives_manifest_reordering() {
	let mut bytes = Vec::new();
	let mut reordered = Vec::new();
	canonical_volume(false).write_to(&mut bytes).unwrap();
	canonical_volume(true).write_to(&mut reordered).unwrap();
	assert_eq!(bytes.len() as u64, IMAGE_BYTES);
	assert_eq!(reordered.len() as u64, IMAGE_BYTES);
	assert_eq!(
		bytes.iter().zip(&reordered).position(|(a, b)| a != b),
		None,
		"first differing byte after reordering"
	);
}

#[test]
#[ignore = "writes the raw fixture for the crate's verification harness"]
fn emit_canonical_volume() {
	let path = env::var_os(OUTPUT_ENV).unwrap();
	let reverse = env::var_os(REVERSE_ENV).is_some();
	let mut image = File::create_new(path).unwrap();
	canonical_volume(reverse).write_to(&mut image).unwrap();
	assert_eq!(image.metadata().unwrap().len(), IMAGE_BYTES);
}

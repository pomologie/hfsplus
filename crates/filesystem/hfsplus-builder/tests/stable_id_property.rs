//! Positive H-V05 coverage over a bounded family of catalog trees.

use hfsplus_builder::volume::{
	CapacityPolicy, EntryKind, EntryMetadata, ForkSource, ManifestEntry, VolumeOptions, plan,
};
use hfsplus_core::ByteLen;
use hfsplus_format::volume::CatalogId;
use hfsplus_reader::{Volume, catalog::EntryKind as StoredKind};

const LABEL: &str = "Stable catalog property";
const BLOCK_BYTES: u32 = 4096;
const ROOT: CatalogId = CatalogId(2);
const ROOT_PARENT: CatalogId = CatalogId(1);
const FIRST_USER_ID: u32 = 16;
const FOLDER_MODE: u16 = 0o755;
const FILE_MODE: u16 = 0o644;
const PAYLOAD_BYTES: usize = BLOCK_BYTES as usize + 1;
const GENERATED: &str = "a/Layer/Payload";
const REQUIRED_FOLDERS: [&str; 3] = ["A", "A/Layer", "Z"];
const OPTIONAL_FILES: [&str; 6] = ["A-keep", "A/item", "A/Layer/Near", "M", "z/Alpha", "z/End"];
const OPTIONAL_BRANCH: [&str; 2] = ["m-branch", "M-Branch/Child"];

fn folder(path: &'static str) -> ManifestEntry<'static, &'static [u8]> {
	ManifestEntry {
		path,
		metadata: EntryMetadata::new(FOLDER_MODE),
		kind: EntryKind::Folder,
	}
}

fn file(path: &'static str) -> ManifestEntry<'static, &'static [u8]> {
	ManifestEntry {
		path,
		metadata: EntryMetadata::new(FILE_MODE),
		kind: EntryKind::File {
			data: ForkSource::whole(&[] as &[u8]),
			resource: None,
		},
	}
}

fn manifest(file_mask: usize, branch_entries: usize) -> Vec<ManifestEntry<'static, &'static [u8]>> {
	let mut entries = REQUIRED_FOLDERS.into_iter().map(folder).collect::<Vec<_>>();
	entries.push(ManifestEntry {
		path: GENERATED,
		metadata: EntryMetadata::new(FILE_MODE),
		kind: EntryKind::GeneratedFile { resource: None },
	});
	entries.extend(
		OPTIONAL_FILES
			.into_iter()
			.enumerate()
			.filter(|(index, _)| file_mask & (1 << index) != 0)
			.map(|(_, path)| file(path)),
	);
	entries.extend(
		OPTIONAL_BRANCH[..branch_entries]
			.iter()
			.enumerate()
			.map(|(index, path)| if index == 0 { folder(path) } else { file(path) }),
	);
	entries
}

fn verify_layout(
	mut entries: Vec<ManifestEntry<'static, &'static [u8]>>,
	reverse: bool,
	payload_len: usize,
) {
	// Names deliberately use only the ASCII subset where the HFS+ order
	// is case-folded lexicographic order over separate path components.
	let mut paths = entries
		.iter()
		.map(|entry| (entry.path, matches!(entry.kind, EntryKind::Folder)))
		.collect::<Vec<_>>();
	paths.sort_by_cached_key(|(path, _)| {
		path.to_ascii_lowercase()
			.split('/')
			.map(str::to_owned)
			.collect::<Vec<_>>()
	});
	let assigned_id = |index| CatalogId(FIRST_USER_ID + u32::try_from(index).unwrap());
	let id_for = |path: &str| {
		if path.is_empty() {
			return ROOT;
		}
		assigned_id(
			paths
				.iter()
				.position(|(candidate, _)| candidate.eq_ignore_ascii_case(path))
				.unwrap(),
		)
	};
	if reverse {
		entries.reverse();
	}
	let options = VolumeOptions::for_capacity(LABEL, BLOCK_BYTES).unwrap();
	let mut draft = plan(options, entries).unwrap();
	assert_eq!(draft.try_catalog_id("").unwrap(), Some(ROOT));
	assert_eq!(draft.try_inode_id("").unwrap(), Some(ROOT));
	let assert_ids = |draft: &hfsplus_builder::volume::VolumeDraft<'_, &'static [u8]>| {
		for (index, (path, _)) in paths.iter().enumerate() {
			let id = Some(assigned_id(index));
			let folded = path.to_ascii_uppercase();
			assert_eq!(draft.try_catalog_id(&folded).unwrap(), id, "{path}");
			assert_eq!(draft.try_inode_id(&folded).unwrap(), id, "{path}");
		}
	};
	assert_ids(&draft);
	draft
		.bind_generated_data(
			GENERATED,
			ForkSource::whole(&[0; PAYLOAD_BYTES][..payload_len]),
		)
		.unwrap();
	assert_ids(&draft);
	let image = draft
		.seal_with_capacity(CapacityPolicy::Auto {
			free_space: ByteLen(0),
		})
		.unwrap();
	let volume = Volume::open(&image).unwrap();
	let catalog = volume.open_catalog(Default::default()).unwrap();
	let root = catalog.root().unwrap();
	assert_eq!(root.metadata.id, ROOT);
	assert_eq!(root.parent, ROOT_PARENT);
	assert_eq!(volume.header().next_catalog_id, assigned_id(paths.len()));
	assert_eq!(catalog.entry(assigned_id(paths.len())).unwrap(), None);
	for (index, (path, directory)) in paths.iter().enumerate() {
		let id = assigned_id(index);
		let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
		let parent_id = id_for(parent);
		let entry = catalog.entry(id).unwrap().unwrap();
		assert_eq!(entry, catalog.lookup(parent_id, name).unwrap().unwrap());
		assert_eq!(entry.metadata.id, id);
		assert_eq!(entry.parent, parent_id);
		assert!(entry.name.into_iter().eq(name.encode_utf16()));
		match entry.kind {
			StoredKind::Directory { .. } => assert!(*directory, "{path}"),
			StoredKind::File { data, .. } => {
				assert!(!directory, "{path}");
				let expected = if *path == GENERATED { payload_len } else { 0 };
				assert_eq!(data.logical_bytes, expected as u64);
			}
			_ => panic!("unexpected catalog entry kind at {path}"),
		}
	}
}

#[test]
fn catalog_ids_and_threads_match_reference_across_parent_closed_trees() {
	let mut cases = 0;
	for file_mask in 0..(1 << OPTIONAL_FILES.len()) {
		for branch_entries in 0..=OPTIONAL_BRANCH.len() {
			for reverse in [false, true] {
				for payload_len in [0, PAYLOAD_BYTES] {
					verify_layout(manifest(file_mask, branch_entries), reverse, payload_len);
					cases += 1;
				}
			}
		}
	}
	println!("HFS+ property cases checked: {cases}");
}

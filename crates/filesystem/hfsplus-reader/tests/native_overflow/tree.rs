use std::{env, fs, num::NonZeroUsize, path::PathBuf};

use hfsplus_core::{ByteOffset, ReadAt};
use hfsplus_format::{bsd, catalog::Metadata, date, volume::CatalogId};
use hfsplus_reader::{
	Volume,
	attributes::AttributeReadLimits,
	catalog::{Catalog, CatalogLimits, EntryKind, HardLinkLimits, ROOT_ID},
	overflow::{ForkKind, OverflowLimits},
};

use super::{FULL_WITNESS_ENV, assert_native_fork, hosted::FixtureFile};

const DOCUMENTS: &str = "Documents";
const SHARED_ATTRIBUTE: &str = "org.pomologie.fragment.shared";
const FOLDER_ATTRIBUTE: &str = "org.pomologie.fragment.folder";
const EXPECTED_ENTRIES: usize = 5;
const EXPECTED_LINKS: usize = 2;
const SYMLINK_READ_LIMIT: usize = 64;
const MODIFIED_DATE: usize = 1;

struct NativeEntry<'a> {
	path: &'a str,
	inode: u64,
	mode: u16,
	links: u32,
	uid: u32,
	gid: u32,
	mtime: i64,
}

impl<'a> NativeEntry<'a> {
	fn parse(line: &'a str) -> Self {
		let [path, inode, mode, links, uid, gid, mtime] =
			line.split('\t').collect::<Vec<_>>().try_into().unwrap();
		Self {
			path,
			inode: inode.parse().unwrap(),
			mode: mode.parse().unwrap(),
			links: links.parse().unwrap(),
			uid: uid.parse().unwrap(),
			gid: gid.parse().unwrap(),
			mtime: mtime.parse().unwrap(),
		}
	}

	fn check_metadata(&self, metadata: &Metadata) {
		assert_eq!(u64::from(metadata.id.0), self.inode, "{} inode", self.path);
		assert_eq!(metadata.permissions.mode, self.mode, "{} mode", self.path);
		assert_eq!(metadata.permissions.uid, self.uid, "{} uid", self.path);
		assert_eq!(metadata.permissions.gid, self.gid, "{} gid", self.path);
		assert_eq!(
			metadata.dates[MODIFIED_DATE],
			date::from_unix_seconds(self.mtime).unwrap().get(),
			"{} mtime",
			self.path
		);
	}
}

#[test]
#[ignore = "requires the disposable Apple native tree and mounted captures"]
fn native_nested_tree_matches_mounted_entries() {
	let directory = PathBuf::from(env::var_os(FULL_WITNESS_ENV).unwrap());
	let raw = FixtureFile::open(&directory.join("native.cdr")).unwrap();
	let volume = Volume::open(raw).unwrap();
	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	let ledger = fs::read_to_string(directory.join("tree.tsv")).unwrap();
	let rows: Vec<_> = ledger.lines().map(NativeEntry::parse).collect();
	assert_eq!(rows.len(), EXPECTED_ENTRIES);
	let mut shared = Vec::new();
	let mut directories = Vec::new();
	let mut symlinks = 0;
	for row in &rows {
		let mut parent = ROOT_ID;
		let mut found = None;
		for component in row.path.split('/') {
			let units: Vec<_> = component.encode_utf16().collect();
			let entry = catalog.lookup_raw(parent, &units).unwrap().unwrap();
			assert_eq!(entry.parent, parent);
			assert_eq!(entry.name, units);
			assert_eq!(
				catalog.lookup(parent, &component.to_uppercase()).unwrap(),
				Some(entry.clone())
			);
			parent = entry.metadata.id;
			found = Some(entry);
		}
		let entry = found.unwrap();
		match (entry.kind, row.mode & bsd::TYPE_MASK) {
			(EntryKind::Directory { .. }, bsd::DIRECTORY) => {
				row.check_metadata(&entry.metadata);
				directories.push((row.path, entry.metadata.id));
				if row.path.ends_with(DOCUMENTS) {
					assert_attribute(
						&catalog,
						entry.metadata.id,
						FOLDER_ATTRIBUTE,
						&fs::read(directory.join("folder.attr")).unwrap(),
					);
				}
			}
			(EntryKind::File { .. }, bsd::REGULAR_FILE) => {
				let file = catalog
					.regular_file(entry.metadata.id, HardLinkLimits::default())
					.unwrap()
					.unwrap();
				row.check_metadata(file.metadata());
				assert_eq!(file.hard_link().unwrap().count.get(), row.links);
				assert_eq!(file.name(), entry.name);
				assert_attribute(
					&catalog,
					file.id(),
					SHARED_ATTRIBUTE,
					&fs::read(directory.join("shared.attr")).unwrap(),
				);
				for (kind, name) in [
					(ForkKind::Data, "data.bin"),
					(ForkKind::Resource, "resource.bin"),
				] {
					let fork = file
						.open_stored_fork(kind, OverflowLimits::default())
						.unwrap();
					assert_native_fork(&fork, &directory.join(name));
				}
				shared.push((file.id(), file.inode_id()));
			}
			(EntryKind::File { .. }, bsd::SYMLINK) => {
				row.check_metadata(&entry.metadata);
				let symlink = catalog
					.symlink(
						entry.metadata.id,
						NonZeroUsize::new(SYMLINK_READ_LIMIT).unwrap(),
					)
					.unwrap()
					.unwrap();
				assert_eq!(
					symlink.read(OverflowLimits::default()).unwrap(),
					fs::read(directory.join("symlink.bin")).unwrap()
				);
				symlinks += 1;
			}
			_ => panic!("native kind differs for {}", row.path),
		}
	}
	assert_eq!(symlinks, 1);
	assert_eq!(shared.len(), EXPECTED_LINKS);
	assert_ne!(shared[0].0, shared[1].0);
	assert_eq!(shared[0].1, shared[1].1);
	assert_eq!(directories.len(), EXPECTED_LINKS);
	for (path, id) in directories {
		let mut cursor = catalog.read_dir(id).unwrap();
		let page = cursor
			.next_page(NonZeroUsize::new(EXPECTED_ENTRIES).unwrap())
			.unwrap();
		assert!(page.complete);
		let mut names: Vec<_> = page
			.entries
			.iter()
			.map(|entry| String::from_utf16(&entry.name).unwrap())
			.collect();
		let mut expected: Vec<_> = rows
			.iter()
			.filter_map(|row| row.path.strip_prefix(path)?.strip_prefix('/'))
			.filter(|name| !name.contains('/'))
			.map(str::to_owned)
			.collect();
		names.sort_unstable();
		expected.sort_unstable();
		assert_eq!(names, expected);
	}
	println!(
		"Matched native tree names, metadata, hard-link identities, attributes and symlink bytes"
	);
}

fn assert_attribute(
	catalog: &Catalog<'_, FixtureFile>,
	id: CatalogId,
	name: &str,
	expected: &[u8],
) {
	let units: Vec<_> = name.encode_utf16().collect();
	let hard_links = HardLinkLimits::default();
	let limits = AttributeReadLimits::default();
	assert!(
		catalog
			.list_entry_attribute_names(id, hard_links, limits)
			.unwrap()
			.contains(&units)
	);
	let value = catalog
		.open_entry_attribute(id, &units, hard_links, limits)
		.unwrap()
		.unwrap();
	assert_eq!(value.len().0, expected.len() as u64);
	let mut observed = vec![0; expected.len()];
	value.read_exact_at(ByteOffset(0), &mut observed).unwrap();
	assert_eq!(observed, expected);
}

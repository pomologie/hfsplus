use core::convert::Infallible;

use hfsplus_format::{
	catalog::{Metadata, Permissions},
	hard_links::PRIVATE_DIRECTORY_NAME,
	volume::Fork,
};

use super::*;
use crate::catalog::ROOT_PARENT_ID;

#[test]
fn rejects_a_parent_cycle_even_when_every_local_directory_count_matches() {
	let first = CatalogId::FIRST_USER;
	let second = CatalogId(first.0 + 1);
	let mut entries = [
		folder(ROOT_ID, ROOT_PARENT_ID, 0),
		folder(first, second, 1),
		folder(second, first, 1),
	];
	assert!(matches!(
		validate_hierarchy::<Infallible>(&mut entries, 8, FolderCounts::Ignore),
		Err(CheckError::Structure(CatalogProblem::ParentCycle(id))) if id == first
	));
}

#[test]
fn required_folder_counts_include_private_directories_but_exclude_files() {
	let private = CatalogId::FIRST_USER;
	let sibling = CatalogId(private.0 + 1);
	let inode = CatalogId(sibling.0 + 1);
	let records = |root_count, private_count, private_flag| {
		let mut root = folder(ROOT_ID, ROOT_PARENT_ID, 2);
		root.entry.metadata.flags = FLAG_HAS_FOLDER_COUNT;
		root.entry.kind = EntryKind::Directory {
			valence: 2,
			tail_word: root_count,
		};
		let mut hidden = folder(private, ROOT_ID, 1);
		hidden.entry.name = PRIVATE_DIRECTORY_NAME.to_vec();
		hidden.entry.metadata.flags = private_flag;
		hidden.entry.kind = EntryKind::Directory {
			valence: 1,
			tail_word: private_count,
		};
		let mut visible = folder(sibling, ROOT_ID, 0);
		visible.entry.metadata.flags = FLAG_HAS_FOLDER_COUNT;
		let mut backing_file = folder(inode, private, 0);
		backing_file.entry.kind = EntryKind::File {
			data: Fork::empty(),
			resource: Fork::empty(),
		};
		[root, hidden, visible, backing_file]
	};
	let require = |mut entries: [Member; 4]| {
		validate_hierarchy::<Infallible>(&mut entries, 8, FolderCounts::Require)
	};
	assert_eq!(require(records(2, 0, FLAG_HAS_FOLDER_COUNT)).unwrap(), 0);
	assert!(matches!(
		require(records(1, 0, FLAG_HAS_FOLDER_COUNT)),
		Err(CheckError::Structure(CatalogProblem::SubfolderCount {
			id: ROOT_ID,
			declared: 1,
			actual: 2
		}))
	));
	assert!(matches!(
		require(records(2, 1, FLAG_HAS_FOLDER_COUNT)),
		Err(CheckError::Structure(CatalogProblem::SubfolderCount { id, declared: 1, actual: 0 })) if id == private
	));
	assert!(matches!(
		require(records(2, 0, 0)),
		Err(CheckError::Structure(CatalogProblem::MissingFolderCount(id))) if id == private
	));
}

fn folder(id: CatalogId, parent: CatalogId, valence: u32) -> Member {
	Member {
		entry: Entry {
			parent,
			name: Vec::new(),
			metadata: Metadata {
				id,
				flags: 0,
				dates: [0; 5],
				permissions: Permissions {
					uid: 0,
					gid: 0,
					admin_flags: 0,
					owner_flags: 0,
					mode: 0,
					special: 0,
				},
				finder_info: [0; 32],
				encoding: 0,
			},
			kind: EntryKind::Directory {
				valence,
				tail_word: 0,
			},
		},
		stub: None,
		state: State::Unseen,
		parent: None,
		children: 0,
		child_folders: 0,
		referenced_inode: false,
	}
}

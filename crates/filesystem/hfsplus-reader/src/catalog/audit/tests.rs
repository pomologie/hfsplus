use core::convert::Infallible;

use hfsplus_format::catalog::{Metadata, Permissions};

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
		validate_hierarchy::<Infallible>(&mut entries, 8),
		Err(CheckError::Structure(CatalogProblem::ParentCycle(id))) if id == first
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
		referenced_inode: false,
	}
}

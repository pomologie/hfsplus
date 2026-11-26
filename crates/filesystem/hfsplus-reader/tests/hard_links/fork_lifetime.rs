use hfsplus_core::{ByteLen, ByteOffset, ReadAt};
use hfsplus_reader::{
	Volume,
	catalog::{CatalogLimits, HardLinkLimits, ROOT_ID},
	overflow::{ForkKind, OverflowLimits},
};

use super::{
	CHAIN_FREE, FIRST_INODE, FIRST_PAYLOAD, FIRST_RESOURCE, FIRST_STUB, Fixture, NATIVE,
	SECOND_INODE, SECOND_PAYLOAD, assert_fork, named,
};

const FORK_WINDOW_CASES: usize = 4_810;

#[test]
fn native_stored_forks_outlive_the_catalog_lookup() {
	let volume = Volume::open(Fixture::decode(NATIVE)).unwrap();
	let limits = OverflowLimits::default();
	let (data, resource) = {
		let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
		let file = catalog
			.regular_file(FIRST_STUB, HardLinkLimits::default())
			.unwrap()
			.unwrap();
		(
			file.open_stored_fork(ForkKind::Data, limits).unwrap(),
			file.open_stored_fork(ForkKind::Resource, limits).unwrap(),
		)
	};
	assert_fork(&data, FIRST_PAYLOAD);
	assert_fork(&resource, FIRST_RESOURCE);
}

#[test]
fn all_native_alias_fork_windows_match_distinct_payloads() {
	let mut checks = 0;
	for bytes in [NATIVE, CHAIN_FREE] {
		let volume = Volume::open(Fixture::decode(bytes)).unwrap();
		let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
		let nested = named(&catalog, ROOT_ID, "nested");
		let first = [(ROOT_ID, "one"), (ROOT_ID, "two"), (nested, "three")];
		let second = [(ROOT_ID, "four"), (nested, "five")];
		for (inode, aliases, data, resource) in [
			(FIRST_INODE, first.as_slice(), FIRST_PAYLOAD, FIRST_RESOURCE),
			(SECOND_INODE, second.as_slice(), SECOND_PAYLOAD, &[]),
		] {
			for &(parent, name) in aliases {
				let id = named(&catalog, parent, name);
				let file = catalog
					.regular_file(id, HardLinkLimits::default())
					.unwrap()
					.unwrap();
				assert_eq!(file.inode_id(), inode);
				assert_eq!(
					file.hard_link().unwrap().count.get(),
					u32::try_from(aliases.len()).unwrap()
				);
				for (kind, expected) in [(ForkKind::Data, data), (ForkKind::Resource, resource)] {
					let fork = file
						.open_stored_fork(kind, OverflowLimits::default())
						.unwrap();
					assert_eq!(fork.len(), expected.len().into());
					let mut scratch = vec![0; expected.len()];
					for start in 0..=expected.len() {
						for end in start..=expected.len() {
							let observed = &mut scratch[..end - start];
							observed.fill(u8::MAX);
							fork.read_exact_at(ByteOffset(ByteLen::from(start).0), observed)
								.unwrap();
							assert_eq!(
								observed,
								&expected[start..end],
								"{name} {kind:?} {start}..{end}"
							);
							checks += 1;
						}
					}
				}
			}
		}
	}
	assert_eq!(checks, FORK_WINDOW_CASES);
	println!("HFS+ property cases checked: {checks}");
}

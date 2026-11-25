use hfsplus_reader::{
	Volume,
	catalog::{CatalogLimits, HardLinkLimits},
	overflow::{ForkKind, OverflowLimits},
};

use super::{FIRST_PAYLOAD, FIRST_RESOURCE, FIRST_STUB, Fixture, NATIVE, assert_fork};

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

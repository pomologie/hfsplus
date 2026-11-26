use hfsplus_format::{
	Error,
	extents::{ForkKind, KEY_BYTES, Key},
	volume::{AllocationBlock, CatalogId, Extent},
};

#[test]
fn encodes_the_fixed_key_and_zero_pads_unused_extent_slots() {
	let key = Key {
		id: CatalogId(0x0102_0304),
		kind: ForkKind::Resource,
		start_block: 0x0506_0708,
	};
	let extent = Extent {
		start: AllocationBlock(0x0a0b_0c0d),
		blocks: 3,
	};
	let record = key.encode_record(&[extent]).unwrap();
	let used_prefix = [
		0, 10, 0xff, 0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 11, 12, 13, 0, 0, 0, 3,
	];
	assert_eq!(record[..used_prefix.len()], used_prefix);
	assert!(record[used_prefix.len()..].iter().all(|&byte| byte == 0));
	assert_eq!(Key::decode(&record).unwrap(), key);
	assert!(matches!(
		Key::decode(&record[..KEY_BYTES - 1]),
		Err(Error::InvalidNode)
	));
	assert!(matches!(key.encode_record(&[]), Err(Error::InvalidFork)));
}

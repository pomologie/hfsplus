use hfsplus_format::date::{UNIX_SECONDS_RANGE, from_unix_seconds};

#[test]
fn unix_seconds_are_checked_against_the_nonzero_hfs_date_range() {
	let first = *UNIX_SECONDS_RANGE.start();
	let last = *UNIX_SECONDS_RANGE.end();
	assert_eq!(from_unix_seconds(first).unwrap().get(), 1);
	assert_eq!(from_unix_seconds(last).unwrap().get(), u32::MAX);
	assert_eq!(from_unix_seconds(0).unwrap().get(), 2_082_844_800);
	for invalid in [i64::MIN, first - 1, last + 1, i64::MAX] {
		assert_eq!(from_unix_seconds(invalid), None);
	}
}

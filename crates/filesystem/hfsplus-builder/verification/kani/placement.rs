use hfsplus_core::{ByteLen, ByteOffset};

use crate::plan::admit_range;

const MAX_PLACEMENTS: usize = 8;

#[kani::proof]
// The bounded scenario visits at most eight placements plus the loop exit.
#[kani::unwind(10)]
fn placements_cover_exactly_the_admitted_prefix() {
	let placements: [(u64, u64); MAX_PLACEMENTS] = kani::any();
	let image_len: u64 = kani::any();
	let mut actual_end = ByteOffset(0);
	let mut reference_end = 0u128;

	for (offset, length) in placements {
		let reference_next = u128::from(offset) + u128::from(length);
		let accepted = u128::from(offset) == reference_end
			&& length != 0
			&& reference_next <= u128::from(image_len);
		let actual = admit_range(
			actual_end,
			ByteOffset(offset),
			ByteLen(length),
			ByteLen(image_len),
		);
		assert_eq!(actual.is_ok(), accepted);
		let Ok(range) = actual else { break };
		assert_eq!(range.start(), actual_end);
		assert_eq!(u128::from(range.end().0), reference_next);
		actual_end = range.end();
		reference_end = reference_next;
	}
	assert_eq!(u128::from(actual_end.0), reference_end);
	assert_eq!(
		actual_end.0 == image_len,
		reference_end == u128::from(image_len)
	);
	kani::cover!(actual_end.0 != 0 && actual_end.0 == image_len, "complete nonempty prefix");
}

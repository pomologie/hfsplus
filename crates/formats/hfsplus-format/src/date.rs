//! Dates stored as unsigned seconds since 1904-01-01 UTC.

use core::{num::NonZeroU32, ops::RangeInclusive};

const UNIX_EPOCH_OFFSET_SECONDS: i64 = 2_082_844_800;

/// The date field value used for an unspecified date.
pub const MISSING: u32 = 0;

/// Unix seconds that can be encoded without using the unspecified date value.
pub const UNIX_SECONDS_RANGE: RangeInclusive<i64> =
	(1 - UNIX_EPOCH_OFFSET_SECONDS)..=(u32::MAX as i64 - UNIX_EPOCH_OFFSET_SECONDS);

/// Encodes a Unix timestamp as a nonzero HFS+ date.
#[must_use]
pub fn from_unix_seconds(seconds: i64) -> Option<NonZeroU32> {
	let hfs_seconds = seconds.checked_add(UNIX_EPOCH_OFFSET_SECONDS)?;
	NonZeroU32::new(u32::try_from(hfs_seconds).ok()?)
}

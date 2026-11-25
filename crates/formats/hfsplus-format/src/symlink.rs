//! Shared metadata and byte rules for the supported native symbolic link profile.

use core::fmt;

/// Finder file type and creator fields stored on a native symbolic link.
pub const FINDER_TYPE_AND_CREATOR: [u8; 8] = *b"slnkrhap";
/// Maximum target bytes in the supported symbolic link profile.
pub const MAX_SYMLINK_TARGET_BYTES: usize = 1024;

/// A symbolic link target is outside the supported byte profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TargetError {
	/// A target contains no bytes.
	Empty,
	/// A target exceeds the native profile limit.
	TooLong,
	/// A target contains a NUL byte.
	Nul,
}

impl fmt::Display for TargetError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Empty => f.write_str("symbolic link target is empty"),
			Self::TooLong => f.write_str("symbolic link target exceeds the native size limit"),
			Self::Nul => f.write_str("symbolic link target contains a NUL byte"),
		}
	}
}

impl core::error::Error for TargetError {}

/// Admits exact target bytes without interpreting their encoding or path syntax.
///
/// # Errors
/// Reports empty, oversized, or NUL-containing targets.
pub fn validate_target(target: &[u8]) -> Result<(), TargetError> {
	if target.is_empty() {
		return Err(TargetError::Empty);
	}
	if target.len() > MAX_SYMLINK_TARGET_BYTES {
		return Err(TargetError::TooLong);
	}
	if target.contains(&0) {
		return Err(TargetError::Nul);
	}
	Ok(())
}

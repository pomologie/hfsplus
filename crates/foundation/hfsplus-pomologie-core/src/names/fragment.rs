//! Bounded substring matching in an admitted HFS-family name collation.

extern crate alloc;

use alloc::vec::Vec;

use super::{MAX_NAME_UNITS, NameCollation, NameError, normalize};

/// A normalized text fragment for matching exact stored name units.
#[derive(Debug)]
pub struct NameFragment {
	collation: NameCollation,
	weights: Vec<u16>,
}

impl NameFragment {
	/// Maximum UTF-16 length of the decomposed fragment.
	pub const MAX_UNITS: usize = MAX_NAME_UNITS;

	/// Compiles text using historical HFS+ decomposition and the volume collation.
	///
	/// An empty fragment, including one entirely ignored by the collation,
	/// returns `None`.
	///
	/// # Errors
	/// Returns an error if the fragment exceeds the decomposed name limit or
	/// allocating its comparison key fails.
	pub fn new(collation: NameCollation, fragment: &str) -> Result<Option<Self>, NameError> {
		let stored = normalize(fragment)?;
		let weights = collation.key(stored.as_units())?;
		Ok((!weights.is_empty()).then_some(Self { collation, weights }))
	}

	/// Tests a stored UTF-16 name using the same collation as the fragment.
	///
	/// # Errors
	/// Returns an error if the name exceeds the stored name limit or allocating
	/// its comparison key fails.
	pub fn matches(&self, name: &[u16]) -> Result<bool, NameError> {
		let contains = |units: &[u16]| {
			units
				.windows(self.weights.len())
				.any(|window| window == self.weights)
		};
		match self.collation {
			NameCollation::CaseFolded => Ok(contains(&self.collation.key(name)?)),
			NameCollation::Binary if name.len() <= MAX_NAME_UNITS => Ok(contains(name)),
			NameCollation::Binary => Err(NameError::TooLong),
		}
	}
}

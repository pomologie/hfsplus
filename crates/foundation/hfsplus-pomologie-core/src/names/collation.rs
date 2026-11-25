extern crate alloc;

use alloc::vec::Vec;
use core::cmp::Ordering;

use super::{MAX_NAME_UNITS, NameError};

mod ordinal_overrides;

use ordinal_overrides::ORDINAL_OVERRIDES;

const IGNORABLE_WEIGHT: u16 = 0;

/// Ordering of stored HFS-family catalog names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum NameCollation {
	/// Historical HFS+ ordering, also used by case-insensitive HFSX catalogs.
	CaseFolded,
	/// Unsigned UTF-16 code unit ordering, used by binary HFSX catalogs.
	Binary,
}

impl NameCollation {
	/// Compares stored UTF-16 name sequences, including empty thread keys.
	///
	/// On-disk names are already decomposed. A caller comparing host text
	/// with a catalog key must normalize the host text before calling this.
	/// Equivalent spellings in the folded mode compare equal.
	#[must_use]
	pub fn compare(self, left: &[u16], right: &[u16]) -> Ordering {
		match self {
			Self::CaseFolded => folded_ordinals(left).cmp(folded_ordinals(right)),
			Self::Binary => left.cmp(right),
		}
	}

	/// Produces an owned comparison key for a stored name or empty thread key.
	///
	/// Comparing two keys made with the same mode gives the same result as
	/// [`Self::compare`]. Keys contain collation weights. Use the original
	/// units to display a name and normalize host text before making a key.
	///
	/// # Errors
	/// Returns an error for input exceeding the on-disk name length limit or if
	/// the bounded key cannot be allocated.
	pub fn key(self, stored: &[u16]) -> Result<Vec<u16>, NameError> {
		if stored.len() > MAX_NAME_UNITS {
			return Err(NameError::TooLong);
		}
		let mut key = Vec::new();
		key.try_reserve_exact(stored.len())
			.map_err(NameError::Allocation)?;
		match self {
			Self::CaseFolded => key.extend(folded_ordinals(stored)),
			Self::Binary => key.extend_from_slice(stored),
		}
		Ok(key)
	}
}

fn folded_ordinals(units: &[u16]) -> impl Iterator<Item = u16> + '_ {
	units.iter().copied().filter_map(|unit| {
		let weight = ORDINAL_OVERRIDES
			.binary_search_by_key(&unit, |&(original, _)| original)
			.map_or(unit, |index| ORDINAL_OVERRIDES[index].1);
		(weight != IGNORABLE_WEIGHT).then_some(weight)
	})
}

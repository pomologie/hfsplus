use core::cmp::Ordering;

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
}

fn folded_ordinals(units: &[u16]) -> impl Iterator<Item = u16> + '_ {
	units.iter().copied().filter_map(|unit| {
		let weight = ORDINAL_OVERRIDES
			.binary_search_by_key(&unit, |&(original, _)| original)
			.map_or(unit, |index| ORDINAL_OVERRIDES[index].1);
		(weight != IGNORABLE_WEIGHT).then_some(weight)
	})
}

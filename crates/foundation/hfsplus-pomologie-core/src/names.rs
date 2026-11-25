//! Canonical decomposition and historical ordering of HFS+ names.
//!
//! Decomposition produces stored UTF-16 units. Collation compares that storage
//! form without changing it. Name admission and host path validation belong
//! to callers.

extern crate alloc;

use alloc::{collections::TryReserveError, vec::Vec};
use core::{char, error::Error, fmt};

mod collation;
mod fragment;
mod unicode32;

pub use collation::NameCollation;
pub use fragment::NameFragment;

/// Maximum number of UTF-16 code units in an on-disk HFS+ name.
pub const MAX_NAME_UNITS: usize = 255;

const HANGUL_SYLLABLE_BASE: u32 = 0xac00;
const HANGUL_LEADING_JAMO_BASE: u32 = 0x1100;
const HANGUL_VOWEL_JAMO_BASE: u32 = 0x1161;
const HANGUL_TRAILING_JAMO_BASE: u32 = 0x11a7;
const HANGUL_LEADING_COUNT: u32 = 19;
const HANGUL_VOWEL_COUNT: u32 = 21;
const HANGUL_TRAILING_COUNT: u32 = 28;
const HANGUL_VOWEL_TRAILING_COUNT: u32 = HANGUL_VOWEL_COUNT * HANGUL_TRAILING_COUNT;
const HANGUL_SYLLABLE_COUNT: u32 = HANGUL_LEADING_COUNT * HANGUL_VOWEL_TRAILING_COUNT;
const STARTER_CLASS: u8 = 0;
const MAX_UNITS_PER_SCALAR: usize = 2;

/// A canonically decomposed sequence within the HFS+ name length limit.
#[derive(Debug, Eq, PartialEq)]
pub struct NormalizedName(Vec<u16>);

impl NormalizedName {
	/// Borrows the stored UTF-16 code units.
	#[must_use]
	pub fn as_units(&self) -> &[u16] {
		&self.0
	}

	/// Transfers ownership of the stored UTF-16 code units.
	#[must_use]
	pub fn into_units(self) -> Vec<u16> {
		self.0
	}
}

/// A canonical name could not be produced.
#[derive(Debug)]
#[non_exhaustive]
pub enum NameError {
	/// The input contained an unpaired UTF-16 surrogate.
	InvalidUtf16,
	/// Canonical decomposition exceeds the on-disk UTF-16 limit.
	TooLong,
	/// The bounded result could not be allocated.
	Allocation(TryReserveError),
}

impl fmt::Display for NameError {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::InvalidUtf16 => formatter.write_str("invalid UTF-16 in HFS+ name"),
			Self::TooLong => write!(formatter, "HFS+ name exceeds {MAX_NAME_UNITS} UTF-16 units"),
			Self::Allocation(error) => write!(formatter, "cannot allocate HFS+ name: {error}"),
		}
	}
}

impl Error for NameError {
	fn source(&self) -> Option<&(dyn Error + 'static)> {
		match self {
			Self::Allocation(error) => Some(error),
			Self::InvalidUtf16 | Self::TooLong => None,
		}
	}
}

/// Decomposes a Unicode string to the HFS+ Unicode 3.2 storage form.
///
/// # Errors
/// Returns a typed error if the decomposed name exceeds the storage limit or
/// the result cannot be allocated.
pub fn normalize(name: &str) -> Result<NormalizedName, NameError> {
	normalize_scalars(name.chars().map(Ok))
}

/// Decomposes a UTF-16 sequence to the HFS+ Unicode 3.2 storage form.
///
/// # Errors
/// Returns a typed error for invalid UTF-16, a name above the decomposed
/// storage limit, or a failure to allocate the result.
pub fn normalize_utf16(units: &[u16]) -> Result<NormalizedName, NameError> {
	normalize_scalars(
		char::decode_utf16(units.iter().copied())
			.map(|scalar| scalar.map_err(|_| NameError::InvalidUtf16)),
	)
}

fn normalize_scalars(
	scalars: impl Iterator<Item = Result<char, NameError>>,
) -> Result<NormalizedName, NameError> {
	let mut buffer = CanonicalBuffer::new();
	for scalar in scalars {
		buffer.decompose(scalar?)?;
	}
	buffer.finish()
}

#[derive(Clone, Copy)]
struct OrderedScalar {
	value: char,
	class: u8,
}

struct CanonicalBuffer {
	scalars: [OrderedScalar; MAX_NAME_UNITS],
	count: usize,
	units: usize,
}

impl CanonicalBuffer {
	fn new() -> Self {
		Self {
			scalars: [OrderedScalar {
				value: '\0',
				class: STARTER_CLASS,
			}; MAX_NAME_UNITS],
			count: 0,
			units: 0,
		}
	}

	fn decompose(&mut self, scalar: char) -> Result<(), NameError> {
		// Wrapping places scalars below the syllable base outside the admitted
		// offset range. The admitted indexes map only to the three jamo ranges.
		let offset = u32::from(scalar).wrapping_sub(HANGUL_SYLLABLE_BASE);
		if offset < HANGUL_SYLLABLE_COUNT {
			let leading = HANGUL_LEADING_JAMO_BASE + offset / HANGUL_VOWEL_TRAILING_COUNT;
			let vowel = HANGUL_VOWEL_JAMO_BASE
				+ (offset % HANGUL_VOWEL_TRAILING_COUNT) / HANGUL_TRAILING_COUNT;
			let trailing = offset % HANGUL_TRAILING_COUNT;
			self.append(char::from_u32(leading).expect("leading jamo is a Unicode scalar"))?;
			self.append(char::from_u32(vowel).expect("vowel jamo is a Unicode scalar"))?;
			if trailing != 0 {
				let codepoint = HANGUL_TRAILING_JAMO_BASE + trailing;
				self.append(char::from_u32(codepoint).expect("trailing jamo is a Unicode scalar"))?;
			}
			return Ok(());
		}

		if let Ok(index) = unicode32::DECOMPOSITIONS.binary_search_by_key(&scalar, |entry| entry.0)
		{
			for &component in unicode32::DECOMPOSITIONS[index].1 {
				self.append(component)?;
			}
			Ok(())
		} else {
			self.append(scalar)
		}
	}

	fn append(&mut self, value: char) -> Result<(), NameError> {
		let units = self.units + value.len_utf16();
		if units > MAX_NAME_UNITS {
			return Err(NameError::TooLong);
		}
		let class = unicode32::COMBINING_CLASSES
			.binary_search_by_key(&value, |entry| entry.0)
			.map_or(STARTER_CLASS, |index| unicode32::COMBINING_CLASSES[index].1);
		let mut index = self.count;
		if class != STARTER_CLASS {
			// A starter or equal class stops the stable insertion. Scalar values
			// are moved together, so supplementary UTF-16 pairs stay adjacent.
			while index > 0 && self.scalars[index - 1].class > class {
				index -= 1;
			}
			self.scalars.copy_within(index..self.count, index + 1);
		}
		self.scalars[index] = OrderedScalar { value, class };
		self.count += 1;
		self.units = units;
		Ok(())
	}

	fn finish(&self) -> Result<NormalizedName, NameError> {
		let mut units = Vec::new();
		units
			.try_reserve_exact(self.units)
			.map_err(NameError::Allocation)?;
		let mut scalar_units = [0; MAX_UNITS_PER_SCALAR];
		for scalar in &self.scalars[..self.count] {
			units.extend_from_slice(scalar.value.encode_utf16(&mut scalar_units));
		}
		Ok(NormalizedName(units))
	}
}

//! Fixed-size UTF-16 keys for indirect regular-file records.

use core::num::NonZeroU32;

use crate::catalog::RawName;

const INODE_NAME_PREFIX: &[u8] = b"iNode";
const MAX_REFERENCE_DIGITS: usize = 10;
const MAX_INODE_NAME_UNITS: usize = INODE_NAME_PREFIX.len() + MAX_REFERENCE_DIGITS;
const DECIMAL_RADIX: u32 = 10;
const DIGIT_ZERO: u16 = b'0' as u16;
const DIGIT_NINE: u16 = b'9' as u16;

/// A nonzero on-disk link reference, distinct from a catalog node ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LinkReference(NonZeroU32);

impl LinkReference {
	/// Admit a stored reference. Native link-chain volumes also use values
	/// outside the historical allocation range.
	#[must_use]
	pub const fn new(value: u32) -> Option<Self> {
		let Some(value) = NonZeroU32::new(value) else {
			return None;
		};
		Some(Self(value))
	}

	/// Return the stored number.
	#[must_use]
	pub const fn get(self) -> u32 {
		self.0.get()
	}

	/// Encode the private payload name in a fixed-size UTF-16 buffer.
	#[must_use]
	pub fn inode_name(self) -> InodeName {
		let mut units = [0; MAX_INODE_NAME_UNITS];
		let mut start = units.len();
		let mut value = self.get();
		while value != 0 {
			start -= 1;
			units[start] = DIGIT_ZERO + (value % DECIMAL_RADIX) as u16;
			value /= DECIMAL_RADIX;
		}
		start -= INODE_NAME_PREFIX.len();
		for (unit, byte) in units[start..].iter_mut().zip(INODE_NAME_PREFIX) {
			*unit = u16::from(*byte);
		}
		InodeName { units, start }
	}

	/// Parse an exact `iNode<decimal>` key with no leading zero.
	#[must_use]
	pub fn from_inode_name(name: RawName<'_>) -> Option<Self> {
		let mut units = inode_suffix(name)?;
		let first = units.next()?;
		if first == DIGIT_ZERO {
			return None;
		}
		let value = core::iter::once(first)
			.chain(units)
			.try_fold(0u32, |value, unit| {
				if !(DIGIT_ZERO..=DIGIT_NINE).contains(&unit) {
					return None;
				}
				value
					.checked_mul(DECIMAL_RADIX)?
					.checked_add(u32::from(unit - DIGIT_ZERO))
			})?;
		Self::new(value)
	}
}

pub(super) fn inode_suffix(name: RawName<'_>) -> Option<impl Iterator<Item = u16> + '_> {
	let mut units = name.units();
	for prefix in INODE_NAME_PREFIX {
		if units.next()? != u16::from(*prefix) {
			return None;
		}
	}
	Some(units)
}

/// A private payload name formatted on the stack.
#[derive(Debug, Clone, Copy)]
pub struct InodeName {
	units: [u16; MAX_INODE_NAME_UNITS],
	start: usize,
}

impl InodeName {
	/// The UTF-16 units to use for the catalog and thread names.
	#[must_use]
	pub fn units(&self) -> &[u16] {
		&self.units[self.start..]
	}
}

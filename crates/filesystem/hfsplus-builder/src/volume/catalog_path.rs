use std::cmp::Ordering;

use hfsplus_core::names::{MAX_NAME_UNITS, NameCollation, NormalizedName, normalize};

use super::VolumeError;

const PATH_SEPARATOR: char = '/';
const SEPARATOR_UNIT: u16 = PATH_SEPARATOR as u16;
const FORBIDDEN_CHARACTERS: [char; 3] = ['\0', ':', PATH_SEPARATOR];
// macOS converts the NUL symbol on lookup and omits these BMP noncharacters
// when exposing the stored name. They cannot round-trip through native paths.
const UNADDRESSABLE_CHARACTERS: [char; 3] = ['\u{2400}', '\u{fffe}', '\u{ffff}'];
const PRINTABLE_ASCII: std::ops::RangeInclusive<u8> = b' '..=b'~';
const DOT: u16 = '.' as u16;
const EXCLUDED_PATH_COMPONENTS: &[&[u16]] = &[&[], &[DOT], &[DOT, DOT]];
// The file hard-link namespace begins with NULs and is already excluded above.
const DIRECTORY_LINK_ROOT_KEY: &[u16] = &ascii_units(b".HFS+ Private Directory Data\r");
const COLLATION: NameCollation = NameCollation::CaseFolded;

const fn ascii_units<const N: usize>(bytes: &[u8; N]) -> [u16; N] {
	let mut units = [0; N];
	let mut index = 0;
	while index < N {
		units[index] = bytes[index] as u16;
		index += 1;
	}
	units
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NameProfile {
	Ascii,
	Unicode,
}

impl NameProfile {
	pub(super) fn volume_label(self, label: &str) -> Result<NormalizedName, VolumeError> {
		let name = self.component(label)?;
		if COLLATION.compare(name.as_units(), &[]).is_eq() {
			return Err(VolumeError::UnsupportedName);
		}
		Ok(name)
	}

	fn component(self, value: &str) -> Result<NormalizedName, VolumeError> {
		if value.is_empty()
			|| value.contains(FORBIDDEN_CHARACTERS)
			|| value.contains(UNADDRESSABLE_CHARACTERS)
			|| (self == Self::Ascii
				&& (value.len() > MAX_NAME_UNITS
					|| !value.bytes().all(|byte| PRINTABLE_ASCII.contains(&byte))))
		{
			return Err(VolumeError::UnsupportedName);
		}
		normalize(value).map_err(VolumeError::Normalization)
	}
}

/// A finite path of canonical components. Slash is unambiguous because it
/// cannot appear in a stored component of this profile.
#[derive(Debug)]
pub(super) struct NormalizedPath(Vec<u16>);

impl NormalizedPath {
	pub(super) fn new(path: &str, profile: NameProfile) -> Result<Self, VolumeError> {
		let mut units = Vec::new();
		for component in path.split(PATH_SEPARATOR) {
			if component.is_empty() {
				return Err(VolumeError::InvalidPath);
			}
			let normalized = profile.component(component)?;
			let name = normalized.as_units();
			if EXCLUDED_PATH_COMPONENTS
				.iter()
				.any(|excluded| COLLATION.compare(name, excluded).is_eq())
			{
				return Err(VolumeError::InvalidPath);
			}
			let first = units.is_empty();
			if first && COLLATION.compare(name, DIRECTORY_LINK_ROOT_KEY).is_eq() {
				return Err(VolumeError::ReservedPath);
			}
			units.try_reserve(name.len() + usize::from(!first))?;
			if !first {
				units.push(SEPARATOR_UNIT);
			}
			units.extend_from_slice(name);
		}
		Ok(Self(units))
	}

	pub(super) fn units(&self) -> &[u16] {
		&self.0
	}

	pub(super) fn parent(&self) -> Option<&[u16]> {
		self.0
			.iter()
			.rposition(|unit| *unit == SEPARATOR_UNIT)
			.map(|separator| &self.0[..separator])
	}

	pub(super) fn name(&self) -> &[u16] {
		self.0
			.rsplit(|unit| *unit == SEPARATOR_UNIT)
			.next()
			.expect("a canonical path has a leaf name")
	}
}

pub(super) fn compare_paths(left: &[u16], right: &[u16]) -> Ordering {
	let mut right = right.split(|unit| *unit == SEPARATOR_UNIT);
	for left in left.split(|unit| *unit == SEPARATOR_UNIT) {
		let Some(right) = right.next() else {
			return Ordering::Greater;
		};
		let order = COLLATION.compare(left, right);
		if !order.is_eq() {
			return order;
		}
	}
	if right.next().is_some() {
		Ordering::Less
	} else {
		Ordering::Equal
	}
}

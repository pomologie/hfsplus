use hfsplus_format::{
	attributes::{
		InlineRecord, Key, MAX_KEY_LENGTH, MAX_NAME_UNITS, NATIVE_INLINE_MAX_VALUE_BYTES,
		NATIVE_KEY_COMPARE_TYPE, NATIVE_NODE_BYTES,
	},
	btree::{BIG_KEYS_MASK, HFS_BTREE_TYPE, TreeMetadata, VARIABLE_INDEX_KEYS_MASK},
	catalog::FLAG_HAS_ATTRIBUTES,
	volume::CatalogId,
};

use crate::tree::{PackedTree, Record, TreeError, TreeOptions};

use super::VolumeError;

const SPECIAL_ATTRIBUTES: [&str; 3] = [
	"com.apple.FinderInfo",
	"com.apple.ResourceFork",
	"com.apple.system.Security",
];

/// Checks whether this writer can represent an inline attribute's name and size.
/// Validates the attribute without reading its value or requiring a draft volume.
///
/// # Errors
/// Rejects invalid or special names and values requiring fork storage.
pub fn validate_inline_attribute(name: &str, value_len: usize) -> Result<(), VolumeError> {
	let name = name_units(name)?;
	// Inspect codec-level names such as the compressed-file control attribute.
	InlineRecord::encoded_len(name.as_slice(), &[])?;
	if value_len > NATIVE_INLINE_MAX_VALUE_BYTES {
		return Err(hfsplus_format::Error::UnsupportedAttributeSize(value_len).into());
	}
	Ok(())
}

struct NameUnits {
	units: [u16; MAX_NAME_UNITS],
	len: usize,
}

impl NameUnits {
	fn as_slice(&self) -> &[u16] {
		&self.units[..self.len]
	}
}

fn name_units(name: &str) -> Result<NameUnits, VolumeError> {
	if SPECIAL_ATTRIBUTES.contains(&name) {
		return Err(VolumeError::SpecialAttribute);
	}
	let mut units = [0; MAX_NAME_UNITS];
	let mut len = 0;
	for unit in name.encode_utf16() {
		if unit == 0 {
			return Err(VolumeError::InvalidAttributeName);
		}
		*units
			.get_mut(len)
			.ok_or(VolumeError::InvalidAttributeName)? = unit;
		len += 1;
	}
	if len == 0 {
		return Err(VolumeError::InvalidAttributeName);
	}
	Ok(NameUnits { units, len })
}

#[derive(Debug, Default)]
pub(super) struct Attributes {
	records: Vec<EncodedAttribute>,
	record_bytes: usize,
}

#[derive(Debug)]
struct EncodedAttribute {
	bytes: Box<[u8]>,
	key_bytes: usize,
}

impl EncodedAttribute {
	fn key(&self) -> Key<'_> {
		Key::split(&self.bytes)
			.expect("encoded inline attribute always contains its key")
			.0
	}
}

impl Attributes {
	pub(super) fn add(
		&mut self,
		owner: CatalogId,
		name: &str,
		value: &[u8],
		limit: usize,
	) -> Result<(), VolumeError> {
		let units = name_units(name)?;
		let name = units.as_slice();
		let size = InlineRecord::encoded_len(name, value)?;
		let record_bytes = self
			.record_bytes
			.checked_add(size)
			.filter(|total| *total <= limit)
			.ok_or(VolumeError::Limit)?;
		let mut bytes = Vec::new();
		bytes.try_reserve_exact(size)?;
		bytes.resize(size, 0);
		InlineRecord::encode(owner, name, value, &mut bytes)?;
		self.records.try_reserve(1)?;
		self.records.push(EncodedAttribute {
			bytes: bytes.into_boxed_slice(),
			key_bytes: Key::encoded_len(name)?,
		});
		self.record_bytes = record_bytes;
		Ok(())
	}

	pub(super) fn sort(&mut self) -> Result<(), VolumeError> {
		self.records
			.sort_unstable_by(|left, right| left.key().cmp(&right.key()));
		for pair in self.records.windows(2) {
			let first = pair[0].key();
			if first == pair[1].key() {
				return Err(VolumeError::DuplicateAttribute(first.file_id));
			}
		}
		Ok(())
	}

	/// Requires the records to have been sorted by their native key order.
	pub(super) fn owner_flags(&self, id: CatalogId) -> u16 {
		if self
			.records
			.binary_search_by_key(&id, |record| record.key().file_id)
			.is_ok()
		{
			FLAG_HAS_ATTRIBUTES
		} else {
			0
		}
	}

	pub(super) fn pack(
		&self,
		max_nodes: u32,
		byte_budget: usize,
		clump_bytes: u32,
	) -> Result<Option<PackedTree>, VolumeError> {
		if self.records.is_empty() {
			return Ok(None);
		}
		let budget_nodes =
			u32::try_from(byte_budget / usize::from(NATIVE_NODE_BYTES)).unwrap_or(u32::MAX);
		let max_nodes = max_nodes.min(budget_nodes);
		if max_nodes == 0 {
			return Err(TreeError::NodeLimit.into());
		}
		let mut records = Vec::new();
		records.try_reserve_exact(self.records.len())?;
		records.extend(
			self.records
				.iter()
				.map(|record| Record::new(&record.bytes, record.key_bytes)),
		);
		let options = TreeOptions {
			node_bytes: NATIVE_NODE_BYTES,
			max_key_bytes: MAX_KEY_LENGTH as u16,
			max_nodes,
			metadata: TreeMetadata {
				clump_bytes,
				tree_type: HFS_BTREE_TYPE,
				key_compare_type: NATIVE_KEY_COMPARE_TYPE,
				attributes: BIG_KEYS_MASK | VARIABLE_INDEX_KEYS_MASK,
			},
		};
		Ok(Some(PackedTree::build(&records, options)?))
	}
}

use hfsplus_format::{
	catalog::{
		EncodeValue, FLAG_THREAD_EXISTS, Key, Metadata, Permissions, Record as CatalogRecord,
		ThreadKind,
	},
	volume::{CatalogId, Fork, Geometry},
};

use crate::tree::{PackedTree, Record as TreeRecord, TreeOptions};

use super::{EntryMetadata, VolumeError};

const FOLDER_TYPE: u16 = 0o040000;
const RECORDS_PER_ENTRY: usize = 2;
// Catalog names are Unicode. The legacy Pascal-string encoding hint uses its default.
pub(super) const DEFAULT_ENCODING_HINT: u32 = 0;

pub(super) fn folder(
	id: CatalogId,
	input: EntryMetadata,
	valence: u32,
	flags: u16,
) -> EncodeValue<'static> {
	EncodeValue::Folder {
		metadata: metadata(id, input, FOLDER_TYPE, flags),
		valence,
		tail_word: 0,
	}
}

pub(super) fn file(
	id: CatalogId,
	input: EntryMetadata,
	kind: u16,
	data: Fork,
	resource: Fork,
	flags: u16,
) -> EncodeValue<'static> {
	EncodeValue::File {
		metadata: metadata(id, input, kind, FLAG_THREAD_EXISTS | flags),
		data,
		resource,
	}
}

fn metadata(id: CatalogId, input: EntryMetadata, kind: u16, flags: u16) -> Metadata {
	Metadata {
		id,
		flags,
		dates: input.dates,
		permissions: Permissions {
			uid: input.uid,
			gid: input.gid,
			admin_flags: 0,
			owner_flags: 0,
			mode: kind | input.mode,
			special: 0,
		},
		finder_info: input.finder_info,
		encoding: DEFAULT_ENCODING_HINT,
	}
}

pub(super) struct Catalog {
	geometry: Geometry,
	records: Vec<EncodedRecord>,
	remaining_record_bytes: usize,
}

struct EncodedRecord {
	parent_or_thread: CatalogId,
	child: Option<CatalogId>,
	key_width: usize,
	bytes: Box<[u8]>,
}

impl Catalog {
	pub(super) fn new(
		geometry: Geometry,
		entries: usize,
		metadata_limit: usize,
	) -> Result<Self, VolumeError> {
		let record_count = entries
			.checked_add(1)
			.and_then(|including_root| including_root.checked_mul(RECORDS_PER_ENTRY))
			.ok_or(VolumeError::Limit)?;
		let mut records = Vec::new();
		records.try_reserve_exact(record_count)?;
		Ok(Self {
			geometry,
			records,
			remaining_record_bytes: metadata_limit,
		})
	}

	pub(super) fn add_entry(
		&mut self,
		parent: CatalogId,
		id: CatalogId,
		name: &[u16],
		value: EncodeValue<'_>,
		kind: ThreadKind,
	) -> Result<(), VolumeError> {
		self.push(parent, Some(id), name, value)?;
		self.push(id, None, &[], EncodeValue::Thread { kind, parent, name })
	}

	fn push(
		&mut self,
		parent_or_thread: CatalogId,
		child: Option<CatalogId>,
		name: &[u16],
		value: EncodeValue<'_>,
	) -> Result<(), VolumeError> {
		let size = CatalogRecord::encoded_len(name, value)?;
		self.remaining_record_bytes = self
			.remaining_record_bytes
			.checked_sub(size)
			.ok_or(VolumeError::Limit)?;
		let mut bytes = Vec::new();
		bytes.try_reserve_exact(size)?;
		bytes.resize(size, 0);
		CatalogRecord::encode(parent_or_thread, name, value, self.geometry, &mut bytes)?;
		self.records.push(EncodedRecord {
			parent_or_thread,
			child,
			key_width: Key::encoded_len(name)?,
			bytes: bytes.into_boxed_slice(),
		});
		Ok(())
	}

	pub(super) fn pack(mut self, options: TreeOptions) -> Result<PackedTree, VolumeError> {
		// Sibling IDs were assigned in HFS+ collation order. A thread has the
		// empty key, which sorts before all admitted child names.
		self.records
			.sort_unstable_by_key(|record| (record.parent_or_thread, record.child));
		let mut records = Vec::new();
		records.try_reserve_exact(self.records.len())?;
		records.extend(
			self.records
				.iter()
				.map(|entry| TreeRecord::new(&entry.bytes, entry.key_width)),
		);
		Ok(PackedTree::build(&records, options)?)
	}
}

//! Encoding for new catalog keys and leaf records.

use crate::{
	Error,
	binary::put,
	volume::{CatalogId, Fork, Geometry},
};

use super::{
	ACCESS_DATE_OFFSET, ATTRIBUTE_MODIFIED_DATE_OFFSET, BACKUP_DATE_OFFSET, BSD_FLAGS_OFFSET,
	BSD_GROUP_ID_OFFSET, BSD_MODE_OFFSET, BSD_OWNER_ID_OFFSET, BSD_SPECIAL_OFFSET,
	CONTENT_MODIFIED_DATE_OFFSET, CREATED_DATE_OFFSET, FILE_DATA_FORK_OFFSET, FILE_RECORD,
	FILE_RECORD_BYTES, FILE_RESOURCE_FORK_OFFSET, FILE_THREAD_RECORD, FINDER_INFO_OFFSET,
	FLAG_THREAD_EXISTS, FOLDER_RECORD, FOLDER_RECORD_BYTES, FOLDER_TAIL_WORD_OFFSET,
	FOLDER_THREAD_RECORD, FOLDER_VALENCE_OFFSET, KEY_CATALOG_ID_OFFSET, KEY_LENGTH_BYTES,
	KEY_LENGTH_OFFSET, KEY_NAME_OFFSET, KEY_NAME_UNIT_COUNT_OFFSET, Key, MAX_NAME_UNITS, Metadata,
	RECORD_CATALOG_ID_OFFSET, RECORD_FLAGS_OFFSET, RECORD_KIND_OFFSET, Record,
	TEXT_ENCODING_OFFSET, THREAD_NAME_OFFSET, THREAD_NAME_UNIT_COUNT_OFFSET,
	THREAD_PARENT_ID_OFFSET, ThreadKind, UTF16_UNIT_BYTES, write_name,
};

impl Key<'_> {
	/// Return the byte length of the key for raw UTF-16 code units.
	///
	/// # Errors
	/// Returns [`Error::InvalidCatalog`] if the name exceeds the catalog limit.
	pub fn encoded_len(name: &[u16]) -> Result<usize, Error> {
		let units = usize::from(name_unit_count(name)?);
		Ok(KEY_NAME_OFFSET + UTF16_UNIT_BYTES * units)
	}

	/// Encode a leaf or variable-length index key and return the bytes written.
	/// The supplied code units are retained as given, including unpaired surrogates.
	///
	/// # Errors
	/// Returns [`Error::InvalidCatalog`] for an overlong name or [`Error::Truncated`]
	/// if the destination is too short. These errors leave the destination unchanged.
	pub fn encode(
		parent_or_thread: CatalogId,
		name: &[u16],
		destination: &mut [u8],
	) -> Result<usize, Error> {
		let key_len = Self::encoded_len(name)?;
		let key = destination.get_mut(..key_len).ok_or(Error::Truncated)?;
		write_key(parent_or_thread, name, key)?;
		Ok(key_len)
	}
}

impl Metadata {
	fn encode(self, bytes: &mut [u8]) -> Result<(), Error> {
		put(bytes, RECORD_CATALOG_ID_OFFSET, self.id.0.to_be_bytes())?;
		put(bytes, RECORD_FLAGS_OFFSET, self.flags.to_be_bytes())?;
		let date_offsets = [
			CREATED_DATE_OFFSET,
			CONTENT_MODIFIED_DATE_OFFSET,
			ATTRIBUTE_MODIFIED_DATE_OFFSET,
			ACCESS_DATE_OFFSET,
			BACKUP_DATE_OFFSET,
		];
		for (date, offset) in self.dates.into_iter().zip(date_offsets) {
			put(bytes, offset, date.to_be_bytes())?;
		}
		let permissions = self.permissions;
		put(bytes, BSD_OWNER_ID_OFFSET, permissions.uid.to_be_bytes())?;
		put(bytes, BSD_GROUP_ID_OFFSET, permissions.gid.to_be_bytes())?;
		put(
			bytes,
			BSD_FLAGS_OFFSET,
			[permissions.admin_flags, permissions.owner_flags],
		)?;
		put(bytes, BSD_MODE_OFFSET, permissions.mode.to_be_bytes())?;
		put(bytes, BSD_SPECIAL_OFFSET, permissions.special.to_be_bytes())?;
		put(bytes, FINDER_INFO_OFFSET, self.finder_info)?;
		put(bytes, TEXT_ENCODING_OFFSET, self.encoding.to_be_bytes())
	}
}

/// Catalog value to serialize with borrowed, host-endian UTF-16 thread names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EncodeValue<'name> {
	/// Folder metadata and counters.
	Folder {
		/// Common catalog metadata.
		metadata: Metadata,
		/// Number of immediate files and folders.
		valence: u32,
		/// Raw tail word used for a folder count by some variants.
		tail_word: u32,
	},
	/// File metadata and both forks.
	File {
		/// Common catalog metadata, including [`FLAG_THREAD_EXISTS`].
		metadata: Metadata,
		/// Data fork descriptor.
		data: Fork,
		/// Resource fork descriptor.
		resource: Fork,
	},
	/// Reverse lookup thread for a file or folder.
	Thread {
		/// Kind of the target record.
		kind: ThreadKind,
		/// Original parent of the target.
		parent: CatalogId,
		/// Original raw UTF-16 name, which must be nonempty.
		name: &'name [u16],
	},
}

impl EncodeValue<'_> {
	fn encoded_len(self, key_name: &[u16]) -> Result<usize, Error> {
		match self {
			Self::Folder { .. } if !key_name.is_empty() => Ok(FOLDER_RECORD_BYTES),
			Self::File { metadata, .. }
				if !key_name.is_empty() && metadata.flags & FLAG_THREAD_EXISTS != 0 =>
			{
				Ok(FILE_RECORD_BYTES)
			}
			Self::Thread { name, .. } if key_name.is_empty() && !name.is_empty() => {
				let units = usize::from(name_unit_count(name)?);
				Ok(THREAD_NAME_OFFSET + UTF16_UNIT_BYTES * units)
			}
			_ => Err(Error::InvalidCatalog),
		}
	}

	fn encode(self, geometry: Geometry, bytes: &mut [u8]) -> Result<(), Error> {
		match self {
			Self::Folder {
				metadata,
				valence,
				tail_word,
			} => {
				put(bytes, RECORD_KIND_OFFSET, FOLDER_RECORD.to_be_bytes())?;
				metadata.encode(bytes)?;
				put(bytes, FOLDER_VALENCE_OFFSET, valence.to_be_bytes())?;
				put(bytes, FOLDER_TAIL_WORD_OFFSET, tail_word.to_be_bytes())
			}
			Self::File {
				metadata,
				data,
				resource,
			} => {
				let data = data.encode(geometry)?;
				let resource = resource.encode(geometry)?;
				// New file records store both reserved words as zero.
				bytes.fill(0);
				put(bytes, RECORD_KIND_OFFSET, FILE_RECORD.to_be_bytes())?;
				metadata.encode(bytes)?;
				put(bytes, FILE_DATA_FORK_OFFSET, data)?;
				put(bytes, FILE_RESOURCE_FORK_OFFSET, resource)
			}
			Self::Thread { kind, parent, name } => {
				let units = name_unit_count(name)?;
				let tag = match kind {
					ThreadKind::Folder => FOLDER_THREAD_RECORD,
					ThreadKind::File => FILE_THREAD_RECORD,
				};
				// New thread records store the reserved word as zero.
				bytes.fill(0);
				put(bytes, RECORD_KIND_OFFSET, tag.to_be_bytes())?;
				put(bytes, THREAD_PARENT_ID_OFFSET, parent.0.to_be_bytes())?;
				put(bytes, THREAD_NAME_UNIT_COUNT_OFFSET, units.to_be_bytes())?;
				write_name(name, &mut bytes[THREAD_NAME_OFFSET..]);
				Ok(())
			}
		}
	}
}

impl Record<'_> {
	/// Return the combined key and value byte length, without checking embedded forks.
	///
	/// # Errors
	/// Returns [`Error::InvalidCatalog`] for an invalid name, key/value combination,
	/// or a file missing the thread flag.
	pub fn encoded_len(name: &[u16], value: EncodeValue<'_>) -> Result<usize, Error> {
		Ok(Key::encoded_len(name)? + value.encoded_len(name)?)
	}

	/// Encode a fresh folder, file, or thread leaf record and return the bytes written.
	/// New file and thread records use zero for their reserved fields.
	///
	/// # Errors
	/// Rejects invalid key/value combinations, names, fork geometry, and short
	/// destinations. Input validation failures leave the destination unchanged.
	pub fn encode(
		parent_or_thread: CatalogId,
		name: &[u16],
		value: EncodeValue<'_>,
		geometry: Geometry,
		destination: &mut [u8],
	) -> Result<usize, Error> {
		let key_len = Key::encoded_len(name)?;
		let len = key_len + value.encoded_len(name)?;
		let record = destination.get_mut(..len).ok_or(Error::Truncated)?;
		let (key, bytes) = record.split_at_mut(key_len);
		value.encode(geometry, bytes)?;
		write_key(parent_or_thread, name, key)?;
		Ok(len)
	}
}

fn name_unit_count(name: &[u16]) -> Result<u16, Error> {
	if name.len() > MAX_NAME_UNITS {
		return Err(Error::InvalidCatalog);
	}
	u16::try_from(name.len()).map_err(|_| Error::InvalidCatalog)
}

fn write_key(parent_or_thread: CatalogId, name: &[u16], key: &mut [u8]) -> Result<(), Error> {
	let units = name_unit_count(name)?;
	let stored_len =
		u16::try_from(key.len() - KEY_LENGTH_BYTES).map_err(|_| Error::InvalidCatalog)?;
	put(key, KEY_LENGTH_OFFSET, stored_len.to_be_bytes())?;
	put(key, KEY_CATALOG_ID_OFFSET, parent_or_thread.0.to_be_bytes())?;
	put(key, KEY_NAME_UNIT_COUNT_OFFSET, units.to_be_bytes())?;
	write_name(name, &mut key[KEY_NAME_OFFSET..]);
	Ok(())
}

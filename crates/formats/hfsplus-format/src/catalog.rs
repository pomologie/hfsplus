//! Lossless catalog keys, file and folder metadata, and thread records.

use core::mem::size_of;

use crate::{
	Error,
	binary::{field, put, u16_at, u32_at},
	volume::{CatalogId, FORK_BYTES, Fork, Geometry},
};

/// Maximum number of UTF-16 code units in a catalog name.
pub const MAX_NAME_UNITS: usize = 255;
const UTF16_UNIT_BYTES: usize = size_of::<u16>();

const KEY_LENGTH_OFFSET: usize = 0;
const KEY_LENGTH_BYTES: usize = size_of::<u16>();
const KEY_CATALOG_ID_OFFSET: usize = 2;
const KEY_NAME_UNIT_COUNT_OFFSET: usize = 6;
const KEY_NAME_OFFSET: usize = KEY_NAME_UNIT_COUNT_OFFSET + UTF16_UNIT_BYTES;
/// Maximum stored key length in bytes, excluding the key length field.
pub const MAX_KEY_LENGTH: usize =
	KEY_NAME_OFFSET - KEY_LENGTH_BYTES + UTF16_UNIT_BYTES * MAX_NAME_UNITS;

const RECORD_KIND_OFFSET: usize = 0;
const FOLDER_RECORD: u16 = 0x0001;
const FILE_RECORD: u16 = 0x0002;
const FOLDER_THREAD_RECORD: u16 = 0x0003;
const FILE_THREAD_RECORD: u16 = 0x0004;
/// Records that name a file must carry this flag because a thread is required.
pub const FLAG_THREAD_EXISTS: u16 = 0x0002;

const RECORD_FLAGS_OFFSET: usize = 2;
const RECORD_CATALOG_ID_OFFSET: usize = 8;
const CREATED_DATE_OFFSET: usize = 12;
const CONTENT_MODIFIED_DATE_OFFSET: usize = 16;
const ATTRIBUTE_MODIFIED_DATE_OFFSET: usize = 20;
const ACCESS_DATE_OFFSET: usize = 24;
const BACKUP_DATE_OFFSET: usize = 28;
const BSD_OWNER_ID_OFFSET: usize = 32;
const BSD_GROUP_ID_OFFSET: usize = 36;
const BSD_FLAGS_OFFSET: usize = 40;
const BSD_MODE_OFFSET: usize = 42;
const BSD_SPECIAL_OFFSET: usize = 44;
const FINDER_INFO_OFFSET: usize = 48;
const FINDER_INFO_BYTES: usize = 32;
const TEXT_ENCODING_OFFSET: usize = 80;

const FOLDER_RECORD_BYTES: usize = 88;
const FOLDER_VALENCE_OFFSET: usize = 4;
const FOLDER_TAIL_WORD_OFFSET: usize = 84;
const FILE_DATA_FORK_OFFSET: usize = 88;
const FILE_RESOURCE_FORK_OFFSET: usize = FILE_DATA_FORK_OFFSET + FORK_BYTES;
const FILE_RECORD_BYTES: usize = FILE_RESOURCE_FORK_OFFSET + FORK_BYTES;
const THREAD_PARENT_ID_OFFSET: usize = 4;
const THREAD_NAME_UNIT_COUNT_OFFSET: usize = 8;
const THREAD_NAME_OFFSET: usize = THREAD_NAME_UNIT_COUNT_OFFSET + UTF16_UNIT_BYTES;

/// Raw UTF-16 code units stored in a catalog key or thread. This view preserves
/// even ill-formed UTF-16 and does not implement filesystem collation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawName<'bytes>(&'bytes [[u8; UTF16_UNIT_BYTES]]);

impl<'bytes> RawName<'bytes> {
	/// Length in UTF-16 code units.
	#[must_use]
	pub fn len(self) -> usize {
		self.0.len()
	}

	/// Whether the name is empty (the catalog thread key convention).
	#[must_use]
	pub fn is_empty(self) -> bool {
		self.0.is_empty()
	}

	/// Iterate the stored big-endian UTF-16 units losslessly.
	pub fn units(self) -> impl ExactSizeIterator<Item = u16> + 'bytes {
		self.0.iter().copied().map(u16::from_be_bytes)
	}
}

/// A leaf record key. Ordinary items use their parent's CNID and their name;
/// thread keys use the target catalog ID with an empty name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key<'bytes> {
	/// Catalog ID stored in the key.
	pub parent_or_thread: CatalogId,
	/// Stored UTF-16 units, without normalization or case folding.
	pub name: RawName<'bytes>,
}

impl<'bytes> Key<'bytes> {
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

	/// Borrow the catalog leaf key and return the remaining record bytes.
	///
	/// # Errors
	/// Returns an error if the key length and name unit count disagree or exceed the record.
	pub fn split_leaf(bytes: &'bytes [u8]) -> Result<(Self, &'bytes [u8]), Error> {
		let end = usize::from(u16_at(bytes, KEY_LENGTH_OFFSET)?) + KEY_LENGTH_BYTES;
		let units = usize::from(u16_at(bytes, KEY_NAME_UNIT_COUNT_OFFSET)?);
		if end != KEY_NAME_OFFSET + UTF16_UNIT_BYTES * units {
			return Err(Error::InvalidCatalog);
		}
		let (key, data) = bytes.split_at_checked(end).ok_or(Error::Truncated)?;
		let name = decode_name(&key[KEY_NAME_OFFSET..], units)?;
		Ok((
			Self {
				parent_or_thread: CatalogId(u32_at(key, KEY_CATALOG_ID_OFFSET)?),
				name,
			},
			data,
		))
	}
}

/// BSD metadata as stored in the catalog. The `special` union is not interpreted here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions {
	/// User ID.
	pub uid: u32,
	/// Group ID.
	pub gid: u32,
	/// Privileged half of the BSD flags.
	pub admin_flags: u8,
	/// Owner-settable BSD flags.
	pub owner_flags: u8,
	/// Raw BSD file kind and permission bits.
	pub mode: u16,
	/// Raw hard-link, link-count or device union value.
	pub special: u32,
}

/// Metadata common to a file or directory catalog record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metadata {
	/// Identity of the record, distinct from the key's parent ID.
	pub id: CatalogId,
	/// Raw catalog flag bits.
	pub flags: u16,
	/// Creation, content modification, attribute modification, access and backup dates.
	pub dates: [u32; 5],
	/// Raw on-disk BSD metadata.
	pub permissions: Permissions,
	/// Finder user and extended information bytes.
	pub finder_info: [u8; FINDER_INFO_BYTES],
	/// On-disk text encoding hint.
	pub encoding: u32,
}

impl Metadata {
	fn decode(bytes: &[u8]) -> Result<Self, Error> {
		let [admin_flags, owner_flags] = field(bytes, BSD_FLAGS_OFFSET)?;
		Ok(Self {
			id: CatalogId(u32_at(bytes, RECORD_CATALOG_ID_OFFSET)?),
			flags: u16_at(bytes, RECORD_FLAGS_OFFSET)?,
			dates: [
				u32_at(bytes, CREATED_DATE_OFFSET)?,
				u32_at(bytes, CONTENT_MODIFIED_DATE_OFFSET)?,
				u32_at(bytes, ATTRIBUTE_MODIFIED_DATE_OFFSET)?,
				u32_at(bytes, ACCESS_DATE_OFFSET)?,
				u32_at(bytes, BACKUP_DATE_OFFSET)?,
			],
			permissions: Permissions {
				uid: u32_at(bytes, BSD_OWNER_ID_OFFSET)?,
				gid: u32_at(bytes, BSD_GROUP_ID_OFFSET)?,
				admin_flags,
				owner_flags,
				mode: u16_at(bytes, BSD_MODE_OFFSET)?,
				special: u32_at(bytes, BSD_SPECIAL_OFFSET)?,
			},
			finder_info: field(bytes, FINDER_INFO_OFFSET)?,
			encoding: u32_at(bytes, TEXT_ENCODING_OFFSET)?,
		})
	}

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

/// On-disk thread kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadKind {
	/// A thread returning the original folder key.
	Folder,
	/// A thread returning the original file key.
	File,
}

/// One parsed leaf value. The two forks contain only embedded extent descriptors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value<'bytes> {
	/// Folder metadata and direct child count.
	Folder {
		/// Common catalog metadata.
		metadata: Metadata,
		/// Number of immediate files and folders declared by the record.
		valence: u32,
		/// Raw tail word. Modern variants may use this as a folder count.
		tail_word: u32,
	},
	/// File metadata and both forks.
	File {
		/// Common catalog metadata.
		metadata: Metadata,
		/// Normal data fork descriptor.
		data: Fork,
		/// Resource fork descriptor.
		resource: Fork,
	},
	/// Reverse lookup record for a file or folder ID.
	Thread {
		/// Kind of the target catalog record.
		kind: ThreadKind,
		/// Original parent of the target.
		parent: CatalogId,
		/// Name of the target, exactly as stored.
		name: RawName<'bytes>,
	},
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

/// A validated catalog leaf key and value pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record<'bytes> {
	/// Catalog key with the file/folder name or thread ID.
	pub key: Key<'bytes>,
	/// On-disk catalog value.
	pub value: Value<'bytes>,
}

impl<'bytes> Record<'bytes> {
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

	/// Decode a complete leaf record with validated fork references. Catalog
	/// traversal is responsible for verifying matching thread targets and key order.
	///
	/// # Errors
	/// Rejects malformed keys or values, unsupported record kinds and out-of-range forks.
	pub fn decode(bytes: &'bytes [u8], geometry: Geometry) -> Result<Self, Error> {
		let (key, data) = Key::split_leaf(bytes)?;
		let kind = u16_at(data, RECORD_KIND_OFFSET)?;
		let value = match kind {
			FOLDER_RECORD if !key.name.is_empty() && data.len() == FOLDER_RECORD_BYTES => {
				Value::Folder {
					metadata: Metadata::decode(data)?,
					valence: u32_at(data, FOLDER_VALENCE_OFFSET)?,
					tail_word: u32_at(data, FOLDER_TAIL_WORD_OFFSET)?,
				}
			}
			FILE_RECORD if !key.name.is_empty() && data.len() == FILE_RECORD_BYTES => {
				let metadata = Metadata::decode(data)?;
				if metadata.flags & FLAG_THREAD_EXISTS == 0 {
					return Err(Error::InvalidCatalog);
				}
				Value::File {
					metadata,
					data: Fork::decode(
						&data[FILE_DATA_FORK_OFFSET..FILE_RESOURCE_FORK_OFFSET],
						geometry,
					)?,
					resource: Fork::decode(&data[FILE_RESOURCE_FORK_OFFSET..], geometry)?,
				}
			}
			FOLDER_THREAD_RECORD | FILE_THREAD_RECORD if key.name.is_empty() => {
				let units = usize::from(u16_at(data, THREAD_NAME_UNIT_COUNT_OFFSET)?);
				if units == 0 {
					return Err(Error::InvalidCatalog);
				}
				Value::Thread {
					kind: if kind == FOLDER_THREAD_RECORD {
						ThreadKind::Folder
					} else {
						ThreadKind::File
					},
					parent: CatalogId(u32_at(data, THREAD_PARENT_ID_OFFSET)?),
					name: decode_name(&data[THREAD_NAME_OFFSET..], units)?,
				}
			}
			FOLDER_RECORD | FILE_RECORD | FOLDER_THREAD_RECORD | FILE_THREAD_RECORD => {
				return Err(Error::InvalidCatalog);
			}
			other => return Err(Error::UnsupportedCatalogRecord(other)),
		};
		Ok(Self { key, value })
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

fn write_name(name: &[u16], bytes: &mut [u8]) {
	for (output, unit) in bytes
		.as_chunks_mut::<UTF16_UNIT_BYTES>()
		.0
		.iter_mut()
		.zip(name)
	{
		*output = unit.to_be_bytes();
	}
}

fn decode_name(bytes: &[u8], units: usize) -> Result<RawName<'_>, Error> {
	if units > MAX_NAME_UNITS || bytes.len() != units * UTF16_UNIT_BYTES {
		return Err(Error::InvalidCatalog);
	}
	Ok(RawName(bytes.as_chunks::<UTF16_UNIT_BYTES>().0))
}

//! Catalog encodings for regular-file hard links and their private payloads.

use core::{fmt, num::NonZeroU32};

use crate::{
	binary::{put, u32_at},
	bsd,
	catalog::{
		EncodeValue, FLAG_HAS_ATTRIBUTES, FLAG_HAS_FOLDER_COUNT, FLAG_THREAD_EXISTS, Key, Metadata,
		Permissions, Record, Value,
	},
	volume::{CatalogId, Fork, Geometry},
};

mod reference;

use reference::inode_suffix;
pub use reference::{InodeName, LinkReference};

const PRIVATE_NAME_NULL_UNITS: usize = 4;
const PRIVATE_NAME_SUFFIX: &[u8] = b"HFS+ Private Data";
const NO_LINK: CatalogId = CatalogId(0);
const STUB_PERMISSIONS: u16 = 0o444;
const OWNER_IMMUTABLE_FLAG: u8 = 0x02;
const PRIVATE_DIRECTORY_LINK_COUNT: u32 = 1;
const HAS_LINK_CHAIN: u16 = 0x0020;
const INODE_FIRST_LINK_OFFSET: usize = 4;
const FINDER_BYTES: usize = 32;
const FINDER_FLAGS_OFFSET: usize = 8;
const FINDER_VERTICAL_OFFSET: usize = 10;
const FINDER_HORIZONTAL_OFFSET: usize = 12;
const FINDER_LOCK_NAME: u16 = 0x1000;
const FINDER_INVISIBLE: u16 = 0x4000;
const FINDER_INITIALIZED: u16 = 0x0100;
const NATIVE_PRIVATE_ICON_LOCATION: u16 = 0x4000;
const STUB_FINDER_CODE: &[u8; 8] = b"hlnkhfs+";
const NATIVE_STUB_ENCODING_HINT: u32 = 126;
const ROOT_CATALOG_ID: CatalogId = CatalogId(2);

/// The special root directory containing regular-file hard-link payloads.
pub const PRIVATE_DIRECTORY_NAME: [u16; PRIVATE_NAME_NULL_UNITS + PRIVATE_NAME_SUFFIX.len()] = {
	let mut units = [0; PRIVATE_NAME_NULL_UNITS + PRIVATE_NAME_SUFFIX.len()];
	let mut index = 0;
	while index < PRIVATE_NAME_SUFFIX.len() {
		units[PRIVATE_NAME_NULL_UNITS + index] = PRIVATE_NAME_SUFFIX[index] as u16;
		index += 1;
	}
	units
};

/// The lower bound for newly allocated chain-free references in TN1150.
pub const LEGACY_REFERENCE_FIRST: u32 = 100;
/// The upper bound for newly allocated chain-free references in TN1150.
pub const LEGACY_REFERENCE_LAST: u32 = 1_073_741_923;

/// A rejected hard-link catalog record or storage convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
	/// A visible link has invalid metadata or claims allocated storage.
	InvalidStub,
	/// The private payload name, metadata, or link count is invalid.
	InvalidInode,
	/// Catalog IDs in the local chain fields are inconsistent.
	InvalidChain,
	/// A lower-level catalog or fork encoding error.
	Format(crate::Error),
}

impl From<crate::Error> for Error {
	fn from(error: crate::Error) -> Self {
		Self::Format(error)
	}
}

impl fmt::Display for Error {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::InvalidStub => f.write_str("invalid HFS+ hard-link stub"),
			Self::InvalidInode => f.write_str("invalid HFS+ hard-link payload"),
			Self::InvalidChain => f.write_str("invalid HFS+ hard-link chain fields"),
			Self::Format(error) => error.fmt(f),
		}
	}
}

impl core::error::Error for Error {
	fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
		match self {
			Self::Format(error) => Some(error),
			_ => None,
		}
	}
}

/// Local sibling pointers in the native linked-stub profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Neighbors {
	/// Forward pointer stored in the stub's BSD group field.
	pub next: Option<CatalogId>,
	/// Backward pointer stored in the stub's BSD owner field.
	pub previous: Option<CatalogId>,
}

/// A visible catalog record referring to one private regular file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stub {
	/// Reference used to construct the private payload key.
	pub reference: LinkReference,
	/// Native neighbor fields when the link-chain flag is set.
	pub neighbors: Option<Neighbors>,
}

impl Stub {
	/// Classify a distinct catalog file as a hard-link stub.
	/// The caller checks its creation date against the private directory and
	/// resolves the target and chain in the selected catalog.
	/// # Errors
	/// Rejects invalid metadata, allocations, or local chain references.
	pub fn decode(record: &Record<'_>) -> Result<Option<Self>, Error> {
		let Value::File {
			metadata,
			data,
			resource,
		} = record.value
		else {
			return Ok(None);
		};
		if !metadata.finder_info.starts_with(STUB_FINDER_CODE) {
			return Ok(None);
		}
		if metadata.id < CatalogId::FIRST_USER
			|| metadata.flags & FLAG_THREAD_EXISTS == 0
			|| metadata.flags & FLAG_HAS_ATTRIBUTES != 0
			|| metadata.permissions.mode & bsd::TYPE_MASK != bsd::REGULAR_FILE
			|| data.total_blocks != 0
			|| resource.total_blocks != 0
		{
			return Err(Error::InvalidStub);
		}
		let reference =
			LinkReference::new(metadata.permissions.special).ok_or(Error::InvalidStub)?;
		let neighbors = if metadata.flags & HAS_LINK_CHAIN == 0 {
			None
		} else {
			let next = decode_neighbor(CatalogId(metadata.permissions.gid), metadata.id)?;
			let previous = decode_neighbor(CatalogId(metadata.permissions.uid), metadata.id)?;
			Some(validate_neighbors(
				Neighbors { next, previous },
				metadata.id,
			)?)
		};
		Ok(Some(Self {
			reference,
			neighbors,
		}))
	}

	/// Produce the empty-fork catalog value written at one visible path.
	/// Use the private directory's creation date for `created`.
	/// # Errors
	/// Rejects reserved stub IDs, reserved or self-referential chain pointers,
	/// and a pointer used for both neighbors of one stub.
	pub fn catalog_value(self, id: CatalogId, created: u32) -> Result<EncodeValue<'static>, Error> {
		if id < CatalogId::FIRST_USER {
			return Err(Error::InvalidStub);
		}
		let mut finder = finder_info(FINDER_INITIALIZED);
		finder[..STUB_FINDER_CODE.len()].copy_from_slice(STUB_FINDER_CODE);
		let mut metadata = Metadata {
			id,
			flags: FLAG_THREAD_EXISTS,
			dates: [created, created, created, created, 0],
			permissions: Permissions {
				mode: bsd::REGULAR_FILE | STUB_PERMISSIONS,
				special: self.reference.get(),
				owner_flags: OWNER_IMMUTABLE_FLAG,
				..empty_permissions()
			},
			finder_info: finder,
			encoding: 0,
		};
		if let Some(neighbors) = self.neighbors {
			let valid = validate_neighbors(neighbors, id)?;
			metadata.flags |= HAS_LINK_CHAIN;
			metadata.permissions.gid = valid.next.unwrap_or(NO_LINK).0;
			metadata.permissions.uid = valid.previous.unwrap_or(NO_LINK).0;
			metadata.encoding = NATIVE_STUB_ENCODING_HINT;
		}
		Ok(EncodeValue::File {
			metadata,
			data: Fork::empty(),
			resource: Fork::empty(),
		})
	}
}

/// One decoded private file, including its content and real inode metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inode {
	pub reference: LinkReference,
	pub link_count: NonZeroU32,
	/// Head of the linked-stub chain. The chain-free profile leaves it absent.
	pub first_link: Option<CatalogId>,
	pub metadata: Metadata,
	pub data: Fork,
	pub resource: Fork,
}

impl Inode {
	/// Decode a candidate private file with the matching directory parent.
	/// # Errors
	/// Rejects malformed keys, forks, and invalid local inode or head fields.
	pub fn decode(
		bytes: &[u8],
		private_directory: CatalogId,
		geometry: Geometry,
	) -> Result<Option<Self>, Error> {
		let record = Record::decode(bytes, geometry)?;
		if record.key.parent_or_thread != private_directory
			|| inode_suffix(record.key.name).is_none()
		{
			return Ok(None);
		}
		let Value::File {
			metadata,
			data,
			resource,
		} = record.value
		else {
			return Err(Error::InvalidInode);
		};
		validate_inode(metadata)?;
		let reference =
			LinkReference::from_inode_name(record.key.name).ok_or(Error::InvalidInode)?;
		let link_count =
			NonZeroU32::new(metadata.permissions.special).ok_or(Error::InvalidInode)?;
		let first_link = if metadata.flags & HAS_LINK_CHAIN == 0 {
			None
		} else {
			let (_, body) = Key::split_leaf(bytes)?;
			let id = CatalogId(u32_at(body, INODE_FIRST_LINK_OFFSET)?);
			Some(decode_neighbor(id, metadata.id)?.ok_or(Error::InvalidChain)?)
		};
		Ok(Some(Self {
			reference,
			link_count,
			first_link,
			metadata,
			data,
			resource,
		}))
	}

	/// Produce the private file value for the chain-free writer profile.
	/// # Errors
	/// Rejects linked-chain heads or invalid inode metadata.
	pub fn chain_free_value(self) -> Result<EncodeValue<'static>, Error> {
		if self.first_link.is_some() {
			return Err(Error::InvalidChain);
		}
		self.catalog_value()
	}

	/// Encode the full private catalog record, including a native chain head.
	/// # Errors
	/// Rejects invalid inode metadata, private directory IDs, conflicting chain
	/// heads, fork extents, or a short destination.
	pub fn encode(
		self,
		private_directory: CatalogId,
		geometry: Geometry,
		destination: &mut [u8],
	) -> Result<usize, Error> {
		if private_directory < CatalogId::FIRST_USER || private_directory == self.metadata.id {
			return Err(Error::InvalidInode);
		}
		let value = self.catalog_value()?;
		let name = self.reference.inode_name();
		let key_len = Key::encoded_len(name.units())?;
		let len = Record::encode(
			private_directory,
			name.units(),
			value,
			geometry,
			destination,
		)?;
		if let Some(first_link) = self.first_link {
			put(
				destination,
				key_len + INODE_FIRST_LINK_OFFSET,
				first_link.0.to_be_bytes(),
			)?;
		}
		Ok(len)
	}

	fn catalog_value(self) -> Result<EncodeValue<'static>, Error> {
		let mut metadata = self.metadata;
		metadata.flags |= FLAG_THREAD_EXISTS;
		validate_inode(metadata)?;
		metadata.permissions.special = self.link_count.get();
		metadata.flags &= !HAS_LINK_CHAIN;
		if let Some(head) = self.first_link {
			decode_neighbor(head, metadata.id)?.ok_or(Error::InvalidChain)?;
			metadata.flags |= HAS_LINK_CHAIN;
		}
		Ok(EncodeValue::File {
			metadata,
			data: self.data,
			resource: self.resource,
		})
	}
}

/// Construct the hidden regular-file namespace as a root folder value.
/// The catalog key uses [`PRIVATE_DIRECTORY_NAME`] under the root CNID, and
/// the ordinary folder thread must map back to that same key.
/// # Errors
/// Rejects a reserved directory identity.
pub fn private_directory_value(
	id: CatalogId,
	root_created: u32,
	valence: u32,
) -> Result<EncodeValue<'static>, Error> {
	if id < CatalogId::FIRST_USER {
		return Err(Error::InvalidInode);
	}
	let mut finder = finder_info(FINDER_INVISIBLE | FINDER_LOCK_NAME);
	put(
		&mut finder,
		FINDER_VERTICAL_OFFSET,
		NATIVE_PRIVATE_ICON_LOCATION.to_be_bytes(),
	)?;
	put(
		&mut finder,
		FINDER_HORIZONTAL_OFFSET,
		NATIVE_PRIVATE_ICON_LOCATION.to_be_bytes(),
	)?;
	Ok(EncodeValue::Folder {
		metadata: Metadata {
			id,
			flags: FLAG_HAS_FOLDER_COUNT,
			dates: [root_created, 0, 0, 0, 0],
			permissions: Permissions {
				mode: bsd::DIRECTORY,
				special: PRIVATE_DIRECTORY_LINK_COUNT,
				owner_flags: OWNER_IMMUTABLE_FLAG,
				..empty_permissions()
			},
			finder_info: finder,
			encoding: 0,
		},
		valence,
		tail_word: 0,
	})
}

/// Identify the exact raw root key for the regular-file private directory.
#[must_use]
pub fn is_private_directory_key(key: Key<'_>) -> bool {
	key.parent_or_thread == ROOT_CATALOG_ID && key.name.units().eq(PRIVATE_DIRECTORY_NAME)
}

fn decode_neighbor(id: CatalogId, own_id: CatalogId) -> Result<Option<CatalogId>, Error> {
	if id == NO_LINK {
		Ok(None)
	} else if id < CatalogId::FIRST_USER || id == own_id {
		Err(Error::InvalidChain)
	} else {
		Ok(Some(id))
	}
}

fn validate_neighbors(neighbors: Neighbors, id: CatalogId) -> Result<Neighbors, Error> {
	if let Some(next) = neighbors.next {
		decode_neighbor(next, id)?.ok_or(Error::InvalidChain)?;
		if Some(next) == neighbors.previous {
			return Err(Error::InvalidChain);
		}
	}
	if let Some(previous) = neighbors.previous {
		decode_neighbor(previous, id)?.ok_or(Error::InvalidChain)?;
	}
	Ok(neighbors)
}

fn validate_inode(metadata: Metadata) -> Result<(), Error> {
	if metadata.id < CatalogId::FIRST_USER
		|| metadata.flags & FLAG_THREAD_EXISTS == 0
		|| metadata.permissions.mode & bsd::TYPE_MASK != bsd::REGULAR_FILE
		|| metadata.finder_info.starts_with(STUB_FINDER_CODE)
	{
		return Err(Error::InvalidInode);
	}
	Ok(())
}

const fn empty_permissions() -> Permissions {
	Permissions {
		uid: 0,
		gid: 0,
		admin_flags: 0,
		owner_flags: 0,
		mode: 0,
		special: 0,
	}
}

fn finder_info(flags: u16) -> [u8; FINDER_BYTES] {
	let mut bytes = [0; FINDER_BYTES];
	bytes[FINDER_FLAGS_OFFSET..FINDER_VERTICAL_OFFSET].copy_from_slice(&flags.to_be_bytes());
	bytes
}

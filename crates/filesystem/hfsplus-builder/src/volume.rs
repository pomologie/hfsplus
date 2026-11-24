//! Write-once, clean nonjournaled HFS+ images from an ASCII manifest.
//!
//! This profile admits printable ASCII catalog names with ordinary file and
//! folder metadata. It preserves data and resource forks. It rejects names
//! outside this profile until the historical HFS+ comparator is qualified.

use std::{error::Error, fmt};

use hfsplus_core::{ByteLen, ByteOffset, ByteRange, RangeError, ReadAt};
use hfsplus_format::volume::{CatalogId, Geometry};

use crate::{Limits, PlanError, allocation::AllocationError, tree::TreeError};

mod emit;
mod manifest;

pub use manifest::{AsciiVolumeDraft, plan_ascii};

const FINDER_INFO_BYTES: usize = 32;
const ENTRY_DATE_COUNT: usize = 5;
const VOLUME_DATE_COUNT: usize = 4;
const DEFAULT_ENTRY_LIMIT: usize = 262_144;
const DEFAULT_TREE_NODE_LIMIT: u32 = 8192;

/// Volume geometry, root metadata, and resource limits.
#[derive(Debug)]
pub struct VolumeOptions<'a> {
	/// The printable ASCII volume label.
	pub label: &'a str,
	/// Checked raw-image geometry.
	pub geometry: Geometry,
	/// Root folder timestamps, ownership, mode, and Finder fields.
	pub root: EntryMetadata,
	/// Creation, modification, backup, and check times in HFS seconds since 1904.
	pub dates: [u32; VOLUME_DATE_COUNT],
	/// Volume Finder fields, including any caller-supplied volume identifier.
	pub finder_info: [u8; FINDER_INFO_BYTES],
	/// Sealed plan limits for metadata and interval count.
	pub limits: Limits,
	/// Maximum number of manifest entries, excluding the root.
	pub max_entries: usize,
	/// Maximum number of nodes allocated for each metadata B-tree.
	pub max_tree_nodes: u32,
}

impl<'a> VolumeOptions<'a> {
	/// Creates volume options with bounded defaults and zero-valued dates.
	#[must_use]
	pub fn new(label: &'a str, geometry: Geometry) -> Self {
		Self {
			label,
			geometry,
			root: EntryMetadata::new(0o755),
			dates: [0; VOLUME_DATE_COUNT],
			finder_info: [0; FINDER_INFO_BYTES],
			limits: Limits::default(),
			max_entries: DEFAULT_ENTRY_LIMIT,
			max_tree_nodes: DEFAULT_TREE_NODE_LIMIT,
		}
	}
}

/// Caller-provided metadata for a regular file or folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryMetadata {
	/// Numeric owner ID.
	pub uid: u32,
	/// Numeric group ID.
	pub gid: u32,
	/// POSIX permission and special mode bits, excluding the file type.
	pub mode: u16,
	/// Creation, content change, attribute change, access, and backup dates in HFS seconds.
	pub dates: [u32; ENTRY_DATE_COUNT],
	/// Opaque Finder metadata bytes.
	pub finder_info: [u8; FINDER_INFO_BYTES],
}

impl EntryMetadata {
	/// Creates metadata with explicit mode bits and zero-valued remaining fields.
	#[must_use]
	pub const fn new(mode: u16) -> Self {
		Self {
			uid: 0,
			gid: 0,
			mode,
			dates: [0; ENTRY_DATE_COUNT],
			finder_info: [0; FINDER_INFO_BYTES],
		}
	}
}

/// One complete or ranged immutable fork source.
#[derive(Debug)]
pub struct ForkSource<S> {
	source: S,
	range: ByteRange,
}

impl<S: ReadAt> ForkSource<S> {
	/// Admits a bounded subrange of an immutable byte source as the fork payload.
	///
	/// # Errors
	/// Rejects a range whose endpoint overflows or exceeds the source length.
	pub fn new(source: S, offset: ByteOffset, length: ByteLen) -> Result<Self, RangeError> {
		let range = ByteRange::new(offset, length, source.len())?;
		Ok(Self { source, range })
	}

	/// Admits the complete immutable byte source as the fork payload.
	#[must_use]
	pub fn whole(source: S) -> Self {
		let length = source.len();
		let range = ByteRange::new(ByteOffset(0), length, length)
			.expect("a complete source range always fits its own length");
		Self { source, range }
	}
}

/// A path and its explicit file or folder metadata.
#[derive(Debug)]
pub struct ManifestEntry<'a, S> {
	/// Relative catalog path, with `/` as the component separator.
	pub path: &'a str,
	/// Entry metadata, with the file type implied by `kind`.
	pub metadata: EntryMetadata,
	/// The entry's catalog kind and optional forks.
	pub kind: EntryKind<S>,
}

/// An ordinary folder or a file with source-backed forks.
#[derive(Debug)]
pub enum EntryKind<S> {
	/// Folder with separately listed children.
	Folder,
	/// Regular file with exact fork payloads.
	File {
		/// The data fork, which may have length zero.
		data: ForkSource<S>,
		/// Optional resource fork.
		resource: Option<ForkSource<S>>,
	},
	/// A regular file whose data fork will be provided after allocating catalog IDs.
	GeneratedFile {
		/// Optional resource fork.
		resource: Option<ForkSource<S>>,
	},
}

/// The manifest, source geometry, or generated volume could not be admitted.
#[derive(Debug)]
#[non_exhaustive]
pub enum VolumeError {
	/// A component or volume label is outside the printable ASCII profile.
	UnsupportedName,
	/// A path has a leading or trailing slash, empty component, or dot component.
	InvalidPath,
	/// Paths are equivalent under the ASCII HFS+ comparator.
	DuplicatePath,
	/// A path's parent was absent or not a folder.
	InvalidParent,
	/// The entry mode included bits outside the POSIX permission field.
	InvalidMode,
	/// Finder metadata denotes a native indirect node outside this profile.
	UnsupportedFinderInfo,
	/// Generated bytes were not bound before sealing.
	MissingGeneratedData(CatalogId),
	/// The requested path is absent or does not identify an unbound generated file.
	NotAwaitingGeneratedData,
	/// An entry, catalog ID, or metadata bound was exceeded.
	Limit,
	/// Memory could not be reserved within the admitted metadata limits.
	Memory(std::collections::TryReserveError),
	/// The block allocator rejected a fork or bitmap.
	Allocation(AllocationError),
	/// A filesystem structure could not be encoded.
	Format(hfsplus_format::Error),
	/// A metadata B-tree could not be packed.
	Tree(TreeError),
	/// The image placements could not be sealed.
	Plan(PlanError),
}

impl fmt::Display for VolumeError {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::UnsupportedName => formatter.write_str("name is outside the HFS+ ASCII profile"),
			Self::InvalidPath => formatter.write_str("invalid relative HFS+ catalog path"),
			Self::DuplicatePath => formatter.write_str("case-equivalent HFS+ catalog paths"),
			Self::InvalidParent => {
				formatter.write_str("HFS+ catalog parent is missing or not a folder")
			}
			Self::InvalidMode => {
				formatter.write_str("HFS+ manifest mode includes non-permission bits")
			}
			Self::UnsupportedFinderInfo => {
				formatter.write_str("native indirect node is outside the HFS+ manifest profile")
			}
			Self::MissingGeneratedData(id) => write!(
				formatter,
				"HFS+ catalog ID {} has no generated data fork",
				id.0
			),
			Self::NotAwaitingGeneratedData => {
				formatter.write_str("path does not identify an unbound generated file")
			}
			Self::Limit => formatter.write_str("HFS+ volume exceeds a manifest or metadata limit"),
			Self::Memory(error) => {
				write!(formatter, "cannot reserve HFS+ volume metadata: {error}")
			}
			Self::Allocation(error) => error.fmt(formatter),
			Self::Format(error) => error.fmt(formatter),
			Self::Tree(error) => error.fmt(formatter),
			Self::Plan(error) => error.fmt(formatter),
		}
	}
}

impl Error for VolumeError {
	fn source(&self) -> Option<&(dyn Error + 'static)> {
		match self {
			Self::Memory(error) => Some(error),
			Self::Allocation(error) => Some(error),
			Self::Format(error) => Some(error),
			Self::Tree(error) => Some(error),
			Self::Plan(error) => Some(error),
			_ => None,
		}
	}
}

impl From<std::collections::TryReserveError> for VolumeError {
	fn from(error: std::collections::TryReserveError) -> Self {
		Self::Memory(error)
	}
}

impl From<AllocationError> for VolumeError {
	fn from(error: AllocationError) -> Self {
		Self::Allocation(error)
	}
}

impl From<hfsplus_format::Error> for VolumeError {
	fn from(error: hfsplus_format::Error) -> Self {
		Self::Format(error)
	}
}

impl From<TreeError> for VolumeError {
	fn from(error: TreeError) -> Self {
		Self::Tree(error)
	}
}

impl From<PlanError> for VolumeError {
	fn from(error: PlanError) -> Self {
		Self::Plan(error)
	}
}

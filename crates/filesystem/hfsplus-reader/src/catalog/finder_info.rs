//! FinderInfo as exposed by macOS, with catalog-private fields masked.

use core::{fmt, ops::Range};

use hfsplus_core::ReadAt;
use hfsplus_format::{
	bsd::{REGULAR_FILE, SYMLINK, TYPE_MASK},
	catalog::FINDER_INFO_BYTES,
	symlink::FINDER_TYPE_AND_CREATOR,
	volume::CatalogId,
};

use super::{Catalog, CatalogError, Entry, EntryKind, HardLinkError, HardLinkLimits};

const FILE_TYPE_AND_CREATOR: Range<usize> = 0..FINDER_TYPE_AND_CREATOR.len();
const DOCUMENT_ID_AND_DATE_ADDED: Range<usize> = 16..24;
const WRITE_GENERATION: Range<usize> = 28..FINDER_INFO_BYTES;
const EMPTY_FINDER_INFO: [u8; FINDER_INFO_BYTES] = [0; FINDER_INFO_BYTES];

/// The effective macOS-visible FinderInfo could not be determined.
#[derive(Debug)]
#[non_exhaustive]
pub enum FinderInfoError<E> {
	/// The visible catalog entry could not be looked up.
	Catalog(CatalogError<E>),
	/// The requested catalog ID is absent or belongs to private metadata.
	MissingEntry(CatalogId),
	/// Resolution of the effective regular-file inode failed.
	HardLink(HardLinkError<E>),
	/// The visible file was absent when resolving its effective metadata.
	MissingRegularFile(CatalogId),
}

impl<E: fmt::Display> fmt::Display for FinderInfoError<E> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Catalog(error) => error.fmt(f),
			Self::MissingEntry(id) => write!(f, "FinderInfo owner {} is unavailable", id.0),
			Self::HardLink(error) => error.fmt(f),
			Self::MissingRegularFile(id) => {
				write!(f, "FinderInfo regular-file owner {} is unavailable", id.0)
			}
		}
	}
}

impl<E: core::error::Error + 'static> core::error::Error for FinderInfoError<E> {
	fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
		match self {
			Self::Catalog(error) => Some(error),
			Self::HardLink(error) => Some(error),
			Self::MissingEntry(_) | Self::MissingRegularFile(_) => None,
		}
	}
}

impl<S: ReadAt> Catalog<'_, S> {
	/// Returns the FinderInfo bytes visible through macOS for a public catalog entry.
	/// An all-zero projection is treated as an absent attribute. A hard-link stub
	/// uses the resolved inode's metadata and a symbolic link uses its own record.
	/// Exact stored bytes remain available through [`Catalog::entry`] and
	/// [`super::RegularFile::metadata`].
	///
	/// # Errors
	/// Reports missing or private catalog IDs, failed catalog or link validation,
	/// and resource limits. Does not read either fork or follow symbolic links.
	pub fn macos_finder_info(
		&self,
		id: CatalogId,
		limits: HardLinkLimits,
	) -> Result<Option<[u8; FINDER_INFO_BYTES]>, FinderInfoError<S::Error>> {
		let entry = self
			.entry(id)
			.map_err(FinderInfoError::Catalog)?
			.ok_or(FinderInfoError::MissingEntry(id))?;
		let profile = Profile::from_entry(&entry);
		let stored = if profile == Profile::RegularFile {
			self.regular_file(id, limits)
				.map_err(FinderInfoError::HardLink)?
				.ok_or(FinderInfoError::MissingRegularFile(id))?
				.metadata()
				.finder_info
		} else {
			entry.metadata.finder_info
		};
		Ok(profile.project(stored))
	}
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Profile {
	RegularFile,
	Directory,
	Symlink,
	Other,
}

impl Profile {
	fn from_entry(entry: &Entry) -> Self {
		if matches!(entry.kind, EntryKind::Directory { .. }) {
			return Self::Directory;
		}
		match entry.metadata.permissions.mode & TYPE_MASK {
			REGULAR_FILE => Self::RegularFile,
			SYMLINK => Self::Symlink,
			_ => Self::Other,
		}
	}

	fn project(self, mut stored: [u8; FINDER_INFO_BYTES]) -> Option<[u8; FINDER_INFO_BYTES]> {
		if self != Self::Other {
			stored[DOCUMENT_ID_AND_DATE_ADDED].fill(0);
			stored[WRITE_GENERATION].fill(0);
		}
		if self == Self::Symlink {
			stored[FILE_TYPE_AND_CREATOR].fill(0);
		}
		(stored != EMPTY_FINDER_INFO).then_some(stored)
	}
}

#[cfg(test)]
mod tests;

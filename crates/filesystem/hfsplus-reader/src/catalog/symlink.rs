use core::{fmt, num::NonZeroUsize};

use hfsplus_core::{ByteOffset, ReadAt};
use hfsplus_format::{
	bsd,
	symlink::{FINDER_TYPE_AND_CREATOR, MAX_SYMLINK_TARGET_BYTES, TargetError, validate_target},
	volume::{CatalogId, Fork},
};

use crate::{
	Volume,
	alloc::{collections::TryReserveError, vec::Vec},
	overflow::{ForkKind, OverflowError, OverflowLimits},
};

use super::{Catalog, CatalogError, EntryKind};

/// A validated symbolic link and its bounded target fork on an opened volume.
#[derive(Debug)]
pub struct Symlink<'volume, S> {
	volume: &'volume Volume<S>,
	id: CatalogId,
	data: Fork,
	target_bytes: NonZeroUsize,
}

/// A symbolic link could not be admitted or read.
#[derive(Debug)]
#[non_exhaustive]
pub enum SymlinkError<E> {
	/// The catalog entry or its ancestry could not be checked.
	Catalog(CatalogError<E>),
	/// The entry does not use the supported native symbolic link profile.
	UnsupportedProfile,
	/// The target length exceeds the effective caller or native byte limit.
	TargetLimit { bytes: u64, max_bytes: usize },
	/// The target length or content is invalid.
	InvalidTarget(TargetError),
	/// The target data fork could not be opened or read.
	Fork(OverflowError<E>),
	/// Storage for the bounded target could not be reserved.
	Allocation(TryReserveError),
}

impl<E: fmt::Display> fmt::Display for SymlinkError<E> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Catalog(error) => error.fmt(f),
			Self::UnsupportedProfile => f.write_str("unsupported HFS+ symbolic link profile"),
			Self::TargetLimit { bytes, max_bytes } => {
				write!(
					f,
					"symbolic link target of {bytes} bytes exceeds the {max_bytes}-byte limit"
				)
			}
			Self::InvalidTarget(error) => error.fmt(f),
			Self::Fork(error) => error.fmt(f),
			Self::Allocation(error) => write!(f, "cannot allocate symbolic link target: {error}"),
		}
	}
}

impl<E: core::error::Error + 'static> core::error::Error for SymlinkError<E> {
	fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
		match self {
			Self::Catalog(error) => Some(error),
			Self::InvalidTarget(error) => Some(error),
			Self::Fork(error) => Some(error),
			Self::Allocation(error) => Some(error),
			Self::UnsupportedProfile | Self::TargetLimit { .. } => None,
		}
	}
}

impl<'volume, S: ReadAt> Catalog<'volume, S> {
	/// Admits a visible catalog entry's native symbolic link metadata and target size.
	///
	/// The effective target limit is the smaller of the caller and native profile limits.
	/// No target content is read yet. Absent and private catalog IDs return `None`.
	///
	/// # Errors
	/// Reports failed catalog checks, unsupported metadata, empty targets, or targets
	/// exceeding the effective byte limit.
	pub fn symlink(
		&self,
		id: CatalogId,
		max_target_bytes: NonZeroUsize,
	) -> Result<Option<Symlink<'volume, S>>, SymlinkError<S::Error>> {
		let Some(entry) = self.entry(id).map_err(SymlinkError::Catalog)? else {
			return Ok(None);
		};
		let EntryKind::File { data, resource } = entry.kind else {
			return Err(SymlinkError::UnsupportedProfile);
		};
		if entry.metadata.permissions.mode & bsd::TYPE_MASK != bsd::SYMLINK
			|| !entry
				.metadata
				.finder_info
				.starts_with(&FINDER_TYPE_AND_CREATOR)
			|| entry.metadata.permissions.owner_flags & bsd::COMPRESSED != 0
			|| resource.logical_bytes != 0
			|| resource.total_blocks != 0
		{
			return Err(SymlinkError::UnsupportedProfile);
		}
		let max_bytes = max_target_bytes.get().min(MAX_SYMLINK_TARGET_BYTES);
		let bytes = data.logical_bytes;
		let size = usize::try_from(bytes)
			.ok()
			.filter(|&size| size <= max_bytes)
			.ok_or(SymlinkError::TargetLimit { bytes, max_bytes })?;
		let target_bytes =
			NonZeroUsize::new(size).ok_or(SymlinkError::InvalidTarget(TargetError::Empty))?;
		Ok(Some(Symlink {
			volume: self.volume,
			id,
			data,
			target_bytes,
		}))
	}
}

impl<S: ReadAt> Symlink<'_, S> {
	/// Returns the admitted target byte count for a caller's aggregate budget.
	#[must_use]
	pub const fn target_bytes(&self) -> NonZeroUsize {
		self.target_bytes
	}

	/// Reads the exact stored target bytes without resolving a referent.
	///
	/// # Errors
	/// Reports invalid target bytes, fork open or read failures, or a failed allocation.
	pub fn read(&self, limits: OverflowLimits) -> Result<Vec<u8>, SymlinkError<S::Error>> {
		let fork = self
			.volume
			.open_fork(self.id, ForkKind::Data, &self.data, limits)
			.map_err(SymlinkError::Fork)?;
		let size = self.target_bytes.get();
		let mut bytes = Vec::new();
		bytes
			.try_reserve_exact(size)
			.map_err(SymlinkError::Allocation)?;
		bytes.resize(size, 0);
		fork.read_exact_at(ByteOffset(0), &mut bytes)
			.map_err(OverflowError::Read)
			.map_err(SymlinkError::Fork)?;
		validate_target(&bytes).map_err(SymlinkError::InvalidTarget)?;
		Ok(bytes)
	}
}

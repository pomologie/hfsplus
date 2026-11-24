use core::ops::Range;

use hfsplus_core::{ByteLen, ByteOffset, ByteRange, ReadAt};
use hfsplus_format::{
	Error as FormatError,
	volume::{ALTERNATE_HEADER_FROM_END, Fork, Geometry, HEADER_BYTES, PRIMARY_HEADER_OFFSET},
};

use crate::{Error, ForkReadSupport, Volume, read_source};

const FIRST_RESERVED_END: u64 = PRIMARY_HEADER_OFFSET + HEADER_BYTES as u64;

/// One of the special files whose fork descriptor lives in the primary header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecialFile {
	/// Allocation bitmap.
	Allocation,
	/// Extents overflow B-tree.
	Extents,
	/// Catalog B-tree.
	Catalog,
	/// Attributes B-tree.
	Attributes,
	/// Startup file.
	Startup,
}

impl<S: ReadAt> Volume<S> {
	/// Opens a special file using the primary volume header's embedded extents.
	///
	/// # Errors
	/// Returns an error for an unsupported volume or fork layout.
	pub fn open_special_fork(
		&self,
		file: SpecialFile,
	) -> Result<ForkReader<'_, S>, Error<S::Error>> {
		let forks = &self.primary.forks;
		let fork = match file {
			SpecialFile::Allocation => &forks.allocation,
			SpecialFile::Extents => &forks.extents,
			SpecialFile::Catalog => &forks.catalog,
			SpecialFile::Attributes => &forks.attributes,
			SpecialFile::Startup => &forks.startup,
		};
		self.open_embedded_fork(fork)
	}

	/// Opens stored bytes through a fork descriptor supplied by the caller.
	/// Bytes beyond the descriptor's logical end, including allocation slack,
	/// are excluded. The caller provides a descriptor from this volume.
	///
	/// # Errors
	/// Returns an error for an unsupported volume, missing overflow extents,
	/// inconsistent fork size, or any extent outside the data allocation area.
	pub fn open_embedded_fork(&self, fork: &Fork) -> Result<ForkReader<'_, S>, Error<S::Error>> {
		let support = self.fork_read_support();
		if support != ForkReadSupport::Supported {
			return Err(Error::UnsupportedVolume(support));
		}
		fork.validate_geometry(self.primary.geometry)
			.map_err(Error::Format)?;
		if fork.has_overflow() {
			return Err(Error::OverflowExtents);
		}
		validate_data_area(fork, self.primary.geometry).map_err(Error::Format)?;
		Ok(ForkReader {
			source: &self.source,
			fork: *fork,
			geometry: self.primary.geometry,
		})
	}
}

/// Bounded positional access to the stored bytes of an embedded HFS+ fork.
/// The fork uses up to eight extents and performs no implicit zero filling.
#[derive(Debug)]
pub struct ForkReader<'a, S> {
	source: &'a S,
	fork: Fork,
	geometry: Geometry,
}

impl<S: ReadAt> ReadAt for ForkReader<'_, S> {
	type Error = Error<S::Error>;

	fn len(&self) -> ByteLen {
		ByteLen(self.fork.logical_bytes)
	}

	fn read_exact_at(
		&self,
		offset: ByteOffset,
		mut destination: &mut [u8],
	) -> Result<(), Self::Error> {
		ByteRange::new(offset, destination.len().into(), self.len()).map_err(Error::Bounds)?;
		let mut relative = offset.0;
		for extent in self.fork.extents() {
			if destination.is_empty() {
				break;
			}
			let bounds = self
				.geometry
				.block_range(extent.start, extent.blocks)
				.map_err(Error::Format)?;
			let extent_len = range_len(&bounds);
			if relative >= extent_len {
				relative -= extent_len;
				continue;
			}
			let count = ByteLen::from(destination.len())
				.0
				.min(extent_len - relative);
			let count =
				usize::try_from(count).map_err(|_| Error::Format(FormatError::InvalidFork))?;
			let (chunk, pending) = destination.split_at_mut(count);
			// relative is strictly below the length of the admitted extent.
			read_source(self.source, ByteOffset(bounds.start + relative), chunk)?;
			destination = pending;
			relative = 0;
		}
		if destination.is_empty() {
			Ok(())
		} else {
			Err(Error::Format(FormatError::InvalidFork))
		}
	}
}

fn validate_data_area(fork: &Fork, geometry: Geometry) -> Result<(), FormatError> {
	let block_bytes = u64::from(geometry.block_size());
	// TN1150 reserves the final allocation block even when the alternate header
	// falls entirely in the unallocated tail past the last whole block.
	let tail_blocks = ALTERNATE_HEADER_FROM_END.div_ceil(block_bytes);
	let last_usable_byte = geometry.allocation_bytes() - tail_blocks * block_bytes;
	for extent in fork.extents() {
		let range = geometry
			.block_range(extent.start, extent.blocks)
			.map_err(|_| FormatError::InvalidFork)?;
		if range.start < FIRST_RESERVED_END || range.end > last_usable_byte {
			return Err(FormatError::InvalidFork);
		}
	}
	Ok(())
}

fn range_len(range: &Range<u64>) -> u64 {
	range.end - range.start
}

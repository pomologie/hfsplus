//! Portable reads from a caller-bounded HFS+ or HFSX volume.
//!
//! This crate reads volume headers and maps embedded fork extents. It operates
//! on stored fork bytes and does not discover partitions, resolve overflow
//! extents, replay journals, or interpret catalog paths or compressed files.
#![no_std]

use core::fmt;

use hfsplus_core::{ByteLen, ByteOffset, ByteRange, RangeError, ReadAt};
use hfsplus_format::{
	Error as FormatError,
	volume::{HEADER_BYTES, Header, PRIMARY_HEADER_OFFSET},
};

mod fork;
mod profile;

pub use fork::{ForkReader, SpecialFile};
pub use profile::ForkReadSupport;

/// Volume discovery or fork I/O failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error<E> {
	/// The requested fork or header bytes lie outside their logical range.
	Bounds(RangeError),
	/// The source failed to fulfill an admitted read.
	Source {
		/// Start of the failed read, in bytes relative to the bounded volume.
		offset: ByteOffset,
		/// Underlying source error, including source mutation if reported.
		error: E,
	},
	/// The required primary header or fork geometry is invalid.
	Format(FormatError),
	/// The volume requires verification or support beyond the embedded-fork profile.
	UnsupportedVolume(ForkReadSupport),
	/// The fork requires extents from the overflow tree.
	OverflowExtents,
}

impl<E: fmt::Display> fmt::Display for Error<E> {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Bounds(error) => error.fmt(formatter),
			Self::Source { offset, error } => {
				write!(
					formatter,
					"cannot read volume at byte {}: {error}",
					offset.0
				)
			}
			Self::Format(error) => error.fmt(formatter),
			Self::UnsupportedVolume(reason) => reason.fmt(formatter),
			Self::OverflowExtents => {
				formatter.write_str("fork requires overflow extent resolution")
			}
		}
	}
}

impl<E: core::error::Error + 'static> core::error::Error for Error<E> {
	fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
		match self {
			Self::Bounds(error) => Some(error),
			Self::Source { error, .. } => Some(error),
			Self::Format(error) => Some(error),
			Self::UnsupportedVolume(_) | Self::OverflowExtents => None,
		}
	}
}

/// Observation of the alternate header at the end of the bounded volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AlternateHeader<'a> {
	/// Both copies were byte-identical and decoded successfully.
	MatchesPrimary,
	/// The alternate decoded successfully but differed from the primary bytes.
	Differs(&'a Header),
	/// The alternate's location was readable and contained only zero bytes.
	Blank,
	/// Nonblank alternate bytes could not be decoded.
	Invalid(FormatError),
}

#[derive(Debug)]
enum MissingAlternate {
	Blank,
	Invalid(FormatError),
}

/// A volume rooted at byte zero of an immutable, bounded source.
#[derive(Debug)]
pub struct Volume<S> {
	source: S,
	primary: Header,
	alternate: Result<Header, MissingAlternate>,
	matches_primary: bool,
}

impl<S: ReadAt> Volume<S> {
	/// Reads both fixed-size volume headers. The primary copy determines the
	/// geometry and remains authoritative for this view. No other structures
	/// are read, and a short or changed source is reported as an I/O failure.
	///
	/// # Errors
	/// Returns an error if the primary cannot be decoded, the source bounds
	/// disagree with the primary allocation geometry, or either read fails.
	/// A readable invalid alternate is reported by [`Self::alternate_header`].
	pub fn open(source: S) -> Result<Self, Error<S::Error>> {
		let source_len = source.len();
		let primary_bytes = read_header(&source, ByteOffset(PRIMARY_HEADER_OFFSET), source_len)?;
		let primary = decode_bounded(&primary_bytes, source_len).map_err(Error::Format)?;
		let alternate_offset = ByteOffset(primary.geometry.alternate_header_offset());
		let alternate_bytes = read_header(&source, alternate_offset, source_len)?;
		let alternate = if alternate_bytes.iter().all(|byte| *byte == 0) {
			Err(MissingAlternate::Blank)
		} else {
			Header::decode(&alternate_bytes, source_len.0).map_err(MissingAlternate::Invalid)
		};
		Ok(Self {
			source,
			primary,
			alternate,
			matches_primary: primary_bytes == alternate_bytes,
		})
	}

	/// Returns the decoded primary header.
	#[must_use]
	pub const fn header(&self) -> &Header {
		&self.primary
	}

	/// Reports whether the alternate is identical, different, blank, or invalid.
	/// A differing alternate can legitimately contain older metadata.
	#[must_use]
	pub const fn alternate_header(&self) -> AlternateHeader<'_> {
		match &self.alternate {
			Ok(_) if self.matches_primary => AlternateHeader::MatchesPrimary,
			Ok(header) => AlternateHeader::Differs(header),
			Err(MissingAlternate::Blank) => AlternateHeader::Blank,
			Err(MissingAlternate::Invalid(error)) => AlternateHeader::Invalid(*error),
		}
	}

	/// Reports whether fork reads are available under the current volume profile.
	#[must_use]
	pub fn fork_read_support(&self) -> ForkReadSupport {
		profile::fork_read_support(&self.primary)
	}
}

fn read_header<S: ReadAt>(
	source: &S,
	offset: ByteOffset,
	source_len: ByteLen,
) -> Result<[u8; HEADER_BYTES], Error<S::Error>> {
	let mut bytes = [0; HEADER_BYTES];
	ByteRange::new(offset, bytes.len().into(), source_len).map_err(Error::Bounds)?;
	read_source(source, offset, &mut bytes)?;
	Ok(bytes)
}

fn decode_bounded(bytes: &[u8; HEADER_BYTES], source_len: ByteLen) -> Result<Header, FormatError> {
	let header = Header::decode(bytes, source_len.0)?;
	if source_len.0 / u64::from(header.geometry.block_size())
		!= u64::from(header.geometry.total_blocks())
	{
		return Err(FormatError::InvalidGeometry);
	}
	Ok(header)
}

fn read_source<S: ReadAt>(
	source: &S,
	offset: ByteOffset,
	destination: &mut [u8],
) -> Result<(), Error<S::Error>> {
	source
		.read_exact_at(offset, destination)
		.map_err(|error| Error::Source { offset, error })
}

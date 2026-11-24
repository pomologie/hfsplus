use std::{error::Error, fmt, io};

use hfsplus_core::{ByteOffset, RangeError, WindowError};

/// An invalid or over-budget image layout.
#[derive(Debug)]
#[non_exhaustive]
pub enum PlanError {
	/// This placement does not begin at the first unassigned image byte.
	OutOfOrder {
		/// First unassigned image byte.
		expected: ByteOffset,
		/// Requested placement offset.
		actual: ByteOffset,
	},
	/// A placement contains no bytes.
	EmptyRange,
	/// The placement exceeds the image.
	ImageRange(RangeError),
	/// The requested source window exceeds its source.
	SourceRange(RangeError),
	/// The constructed prefix leaves a trailing, unassigned image range.
	Incomplete {
		/// First unassigned byte.
		end: ByteOffset,
	},
	/// The placement would exceed the range budget.
	RangeLimit,
	/// The placement would exceed the owned metadata budget.
	MetadataLimit,
	/// Memory for another placement could not be reserved.
	Allocation(std::collections::TryReserveError),
}

impl fmt::Display for PlanError {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::OutOfOrder { expected, actual } => write!(
				formatter,
				"expected placement at byte {}, got {}",
				expected.0, actual.0
			),
			Self::EmptyRange => formatter.write_str("image placement is empty"),
			Self::ImageRange(error) => write!(formatter, "invalid image placement: {error}"),
			Self::SourceRange(error) => write!(formatter, "invalid source window: {error}"),
			Self::Incomplete { end } => write!(
				formatter,
				"image has unassigned bytes beginning at {}",
				end.0
			),
			Self::RangeLimit => formatter.write_str("image exceeds the placement limit"),
			Self::MetadataLimit => formatter.write_str("image exceeds the owned metadata limit"),
			Self::Allocation(error) => write!(formatter, "cannot reserve image placement: {error}"),
		}
	}
}

impl Error for PlanError {
	fn source(&self) -> Option<&(dyn Error + 'static)> {
		match self {
			Self::ImageRange(error) | Self::SourceRange(error) => Some(error),
			Self::Allocation(error) => Some(error),
			_ => None,
		}
	}
}

/// An exact positional read from a sealed image failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum ReadError<E> {
	/// The destination would extend beyond the image.
	Bounds(RangeError),
	/// An immutable source could not supply the requested bytes.
	Source {
		/// Absolute image offset mapped to the failing source read.
		offset: ByteOffset,
		/// Underlying source error.
		error: WindowError<E>,
	},
}

impl<E: fmt::Display> fmt::Display for ReadError<E> {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Bounds(error) => error.fmt(formatter),
			Self::Source { offset, error } => {
				write!(formatter, "cannot read image byte {}: {error}", offset.0)
			}
		}
	}
}

impl<E: Error + 'static> Error for ReadError<E> {
	fn source(&self) -> Option<&(dyn Error + 'static)> {
		match self {
			Self::Bounds(error) => Some(error),
			Self::Source { error, .. } => Some(error),
		}
	}
}

/// A sequential image emission ended without a complete output.
#[derive(Debug)]
#[non_exhaustive]
pub enum WriteError<E> {
	/// The caller requested cancellation before the next output write.
	Cancelled,
	/// An image source could not be read exactly.
	Read(ReadError<E>),
	/// The sink could not accept all bytes.
	Sink(io::Error),
}

impl<E: fmt::Display> fmt::Display for WriteError<E> {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Cancelled => formatter.write_str("image emission was cancelled"),
			Self::Read(error) => error.fmt(formatter),
			Self::Sink(error) => write!(formatter, "cannot write image: {error}"),
		}
	}
}

impl<E: Error + 'static> Error for WriteError<E> {
	fn source(&self) -> Option<&(dyn Error + 'static)> {
		match self {
			Self::Cancelled => None,
			Self::Read(error) => Some(error),
			Self::Sink(error) => Some(error),
		}
	}
}

//! Portable, exact positional reads for bounded filesystem images.
#![no_std]

use core::fmt;

/// A zero-based byte offset relative to a source or window.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ByteOffset(pub u64);

/// A length measured in bytes.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ByteLen(pub u64);

impl ByteOffset {
	/// Adds a byte length, returning `None` if the endpoint overflows.
	#[must_use]
	pub const fn checked_add(self, len: ByteLen) -> Option<Self> {
		match self.0.checked_add(len.0) {
			Some(end) => Some(Self(end)),
			None => None,
		}
	}
}

impl From<usize> for ByteLen {
	fn from(len: usize) -> Self {
		// The supported pointer widths cannot exceed the disk's 64-bit length type.
		Self(len as u64)
	}
}

/// A byte range could not be admitted into its source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RangeError {
	/// Offset plus length is unrepresentable.
	Overflow,
	/// The endpoint is beyond the source length.
	OutsideSource,
}

impl fmt::Display for RangeError {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Overflow => formatter.write_str("byte range endpoint overflows"),
			Self::OutsideSource => formatter.write_str("byte range exceeds its source"),
		}
	}
}

impl core::error::Error for RangeError {}

/// An admitted half-open byte range with a representable endpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ByteRange {
	start: ByteOffset,
	end: ByteOffset,
}

impl ByteRange {
	/// Admits a byte range into a source of the given length.
	///
	/// # Errors
	/// Returns an error if the endpoint overflows or exceeds `source_len`.
	pub const fn new(
		start: ByteOffset,
		len: ByteLen,
		source_len: ByteLen,
	) -> Result<Self, RangeError> {
		let Some(end) = start.checked_add(len) else {
			return Err(RangeError::Overflow);
		};
		if end.0 > source_len.0 {
			return Err(RangeError::OutsideSource);
		}
		Ok(Self { start, end })
	}

	/// Returns the included start offset.
	#[must_use]
	pub const fn start(self) -> ByteOffset {
		self.start
	}

	/// Returns the excluded end offset.
	#[must_use]
	pub const fn end(self) -> ByteOffset {
		self.end
	}

	/// Returns the byte length.
	#[must_use]
	pub const fn len(self) -> ByteLen {
		ByteLen(self.end.0 - self.start.0)
	}

	/// Returns whether the range contains no bytes.
	#[must_use]
	pub const fn is_empty(self) -> bool {
		self.start.0 == self.end.0
	}
}

/// A finite immutable view of a byte-addressed image.
///
/// Sources backed by mutable storage must detect changes affecting their view
/// and report a source error. A successful read initializes the entire slice.
/// The destination's contents after a read error are unspecified.
pub trait ReadAt {
	/// A bounds, source-change, or I/O error.
	type Error;

	/// Returns the fixed byte length of the view.
	fn len(&self) -> ByteLen;

	/// Returns whether the view contains no bytes.
	fn is_empty(&self) -> bool {
		self.len() == ByteLen(0)
	}

	/// Reads exactly the requested bytes. An empty read at EOF succeeds.
	///
	/// # Errors
	/// Returns an error for overflowing or out-of-bounds ranges and when the
	/// bytes cannot be read exactly from the immutable view.
	fn read_exact_at(&self, offset: ByteOffset, destination: &mut [u8]) -> Result<(), Self::Error>;
}

impl<S: ReadAt + ?Sized> ReadAt for &S {
	type Error = S::Error;

	fn len(&self) -> ByteLen {
		(**self).len()
	}

	fn read_exact_at(&self, offset: ByteOffset, destination: &mut [u8]) -> Result<(), Self::Error> {
		(**self).read_exact_at(offset, destination)
	}
}

impl ReadAt for [u8] {
	type Error = RangeError;

	fn len(&self) -> ByteLen {
		self.len().into()
	}

	fn read_exact_at(&self, offset: ByteOffset, destination: &mut [u8]) -> Result<(), Self::Error> {
		let range = ByteRange::new(offset, destination.len().into(), ReadAt::len(self))?;
		// Admission bounds both offsets by this usize-sized slice's length.
		destination.copy_from_slice(&self[range.start.0 as usize..range.end.0 as usize]);
		Ok(())
	}
}

/// A read from a bounded subview failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum WindowError<E> {
	/// The request exceeds the window.
	Bounds(RangeError),
	/// The parent source could not fulfill an admitted request.
	Source(E),
}

impl<E: fmt::Display> fmt::Display for WindowError<E> {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Bounds(error) => error.fmt(formatter),
			Self::Source(error) => write!(formatter, "cannot read parent source: {error}"),
		}
	}
}

impl<E: core::error::Error + 'static> core::error::Error for WindowError<E> {
	fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
		match self {
			Self::Bounds(error) => Some(error),
			Self::Source(error) => Some(error),
		}
	}
}

/// A bounded subview whose byte zero is the start of the admitted range.
#[derive(Debug)]
pub struct Window<S> {
	source: S,
	range: ByteRange,
}

impl<S: ReadAt> Window<S> {
	/// Creates a subview over a range in `source`.
	///
	/// # Errors
	/// Returns an error if the endpoint overflows or exceeds the source length.
	pub fn new(source: S, offset: ByteOffset, len: ByteLen) -> Result<Self, RangeError> {
		let range = ByteRange::new(offset, len, source.len())?;
		Ok(Self { source, range })
	}
}

impl<S: ReadAt> ReadAt for Window<S> {
	type Error = WindowError<S::Error>;

	fn len(&self) -> ByteLen {
		self.range.len()
	}

	fn read_exact_at(&self, offset: ByteOffset, destination: &mut [u8]) -> Result<(), Self::Error> {
		ByteRange::new(offset, destination.len().into(), self.len())
			.map_err(WindowError::Bounds)?;
		// The relative offset cannot exceed the already admitted window length.
		let absolute = ByteOffset(self.range.start().0 + offset.0);
		self.source
			.read_exact_at(absolute, destination)
			.map_err(WindowError::Source)
	}
}

#[cfg(test)]
mod tests {
	use super::{ByteLen, ByteOffset, ByteRange, RangeError, ReadAt, Window, WindowError};

	#[test]
	fn rejects_range_overflow_and_reads_past_eof() {
		assert_eq!(
			ByteRange::new(ByteOffset(u64::MAX), ByteLen(1), ByteLen(u64::MAX)),
			Err(RangeError::Overflow)
		);
		assert_eq!(
			b"image".as_slice().read_exact_at(ByteOffset(5), &mut [0]),
			Err(RangeError::OutsideSource)
		);
		assert!(
			b"image"
				.as_slice()
				.read_exact_at(ByteOffset(5), &mut [])
				.is_ok()
		);
	}

	#[test]
	fn nested_windows_keep_relative_bounds() {
		let window = Window::new(b"image".as_slice(), ByteOffset(1), ByteLen(3)).unwrap();
		let window = Window::new(window, ByteOffset(1), ByteLen(2)).unwrap();
		let mut bytes = [0; 2];
		window.read_exact_at(ByteOffset(0), &mut bytes).unwrap();
		assert_eq!(&bytes, b"ag");
		assert!(matches!(
			window.read_exact_at(ByteOffset(2), &mut bytes),
			Err(WindowError::Bounds(RangeError::OutsideSource))
		));
		assert!(window.read_exact_at(ByteOffset(2), &mut []).is_ok());
	}

	#[test]
	fn preserves_parent_failure() {
		struct FailingSource;
		impl ReadAt for FailingSource {
			type Error = &'static str;
			fn len(&self) -> ByteLen {
				ByteLen(1)
			}
			fn read_exact_at(&self, _: ByteOffset, _: &mut [u8]) -> Result<(), Self::Error> {
				Err("source changed")
			}
		}
		let window = Window::new(FailingSource, ByteOffset(0), ByteLen(1)).unwrap();
		assert!(matches!(
			window.read_exact_at(ByteOffset(0), &mut [0]),
			Err(WindowError::Source("source changed"))
		));
	}
}

use core::fmt;

use crate::{
	byte_range::{ByteLen, ByteOffset, ByteRange, RangeError},
	read_at::ReadAt,
};

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

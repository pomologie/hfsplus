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

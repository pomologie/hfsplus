use crate::byte_range::{ByteLen, ByteOffset, ByteRange, RangeError};

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
		destination.copy_from_slice(&self[range.start().0 as usize..range.end().0 as usize]);
		Ok(())
	}
}

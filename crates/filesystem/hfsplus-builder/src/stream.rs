use std::{
	error::Error,
	io::{self, Read, Write},
};

use hfsplus_core::{ByteLen, ByteOffset, ReadAt};

use crate::{SealedPlan, WriteError};

const COPY_BUFFER_BYTES: usize = 32 * 1024;

impl<S: ReadAt> SealedPlan<S> {
	/// Streams the image to a sequential sink with a fixed-size scratch buffer.
	///
	/// # Errors
	/// Returns the original source or sink error. The sink may contain a prefix
	/// on error. The caller remains responsible for flushing or publishing it.
	pub fn write_to(&self, sink: impl Write) -> Result<(), WriteError<S::Error>> {
		self.write_with_cancel(sink, || false)
	}

	/// Streams the image while checking for cancellation before reads and writes.
	///
	/// # Errors
	/// Aborts on cancellation or source or sink failure. The sink may contain a
	/// prefix on error. The caller remains responsible for flushing or publishing it.
	pub fn write_with_cancel(
		&self,
		mut sink: impl Write,
		mut cancelled: impl FnMut() -> bool,
	) -> Result<(), WriteError<S::Error>> {
		let mut offset = ByteOffset(0);
		let mut buffer = [0; COPY_BUFFER_BYTES];
		while offset.0 < self.image_len().0 {
			if cancelled() {
				return Err(WriteError::Cancelled);
			}
			let count = read_size(self.image_len(), offset, buffer.len());
			let chunk = &mut buffer[..count];
			self.read_exact_at(offset, chunk)
				.map_err(WriteError::Read)?;
			if cancelled() {
				return Err(WriteError::Cancelled);
			}
			sink.write_all(chunk).map_err(WriteError::Sink)?;
			offset.0 += ByteLen::from(count).0;
		}
		Ok(())
	}

	/// Returns a fresh sequential reader starting at byte zero of this image.
	#[must_use]
	pub fn reader(&self) -> ImageReader<'_, S> {
		ImageReader {
			plan: self,
			offset: ByteOffset(0),
		}
	}
}

/// A sequential source suitable for a caller's image-container encoder.
///
/// This adapter borrows the plan and allocates no intermediate image buffer.
#[derive(Debug)]
pub struct ImageReader<'a, S> {
	plan: &'a SealedPlan<S>,
	offset: ByteOffset,
}

impl<S: ReadAt> Read for ImageReader<'_, S>
where
	S::Error: Error + Send + Sync + 'static,
{
	fn read(&mut self, destination: &mut [u8]) -> io::Result<usize> {
		let count = read_size(self.plan.image_len(), self.offset, destination.len());
		self.plan
			.read_exact_at(self.offset, &mut destination[..count])
			.map_err(io::Error::other)?;
		self.offset.0 += ByteLen::from(count).0;
		Ok(count)
	}
}

fn read_size(end: ByteLen, offset: ByteOffset, available: usize) -> usize {
	// The cursor starts at zero and advances at most to the admitted image end.
	// The result is also bounded by the caller's usize-sized buffer.
	(end.0 - offset.0).min(ByteLen::from(available).0) as usize
}

use hfsplus_core::{ByteLen, ByteOffset, ByteRange, ReadAt, Window};

use crate::{PlanError, ReadError};

const DEFAULT_MAX_RANGES: usize = 1_000_000;
const DEFAULT_METADATA_LIMIT: ByteLen = ByteLen(64 * 1024 * 1024);
const IMAGE_START: ByteOffset = ByteOffset(0);

/// Limits on the planner's owned metadata, independent of the image length.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
	/// Maximum number of placements retained by the image.
	pub max_ranges: usize,
	/// Maximum sum of owned metadata payload lengths.
	pub max_metadata_bytes: ByteLen,
}

impl Default for Limits {
	fn default() -> Self {
		Self {
			max_ranges: DEFAULT_MAX_RANGES,
			max_metadata_bytes: DEFAULT_METADATA_LIMIT,
		}
	}
}

/// How a sealed image range is backed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RangeKind {
	/// Owned, immutable filesystem metadata.
	Metadata,
	/// Bytes read from an admitted immutable source window.
	Source,
	/// Explicit zeros that require no backing source.
	Zero,
}

#[derive(Debug)]
enum Payload<S> {
	Metadata(Box<[u8]>),
	Source(Window<S>),
	Zero,
}

#[derive(Debug)]
struct Placement<S> {
	range: ByteRange,
	payload: Payload<S>,
}

/// Assembles a complete image in ascending byte order under fixed limits.
///
/// An explicit offset at each placement keeps allocation mistakes visible. A
/// successful append covers the next bytes exactly. A failed append leaves the
/// planned prefix unchanged. The logical filesystem allocator supplies payloads
/// and represents padding by calling [`Self::push_zero`].
#[derive(Debug)]
pub struct PlanBuilder<S> {
	image_len: ByteLen,
	end: ByteOffset,
	limits: Limits,
	metadata_bytes: ByteLen,
	placements: Vec<Placement<S>>,
}

impl<S: ReadAt> PlanBuilder<S> {
	/// Starts an empty plan for exactly `image_len` bytes.
	#[must_use]
	pub const fn new(image_len: ByteLen, limits: Limits) -> Self {
		Self {
			image_len,
			end: IMAGE_START,
			limits,
			metadata_bytes: ByteLen(0),
			placements: Vec::new(),
		}
	}

	/// Places owned filesystem metadata at `offset`.
	///
	/// # Errors
	/// Rejects empty, overlapping, gapped, oversized or over-budget placements.
	pub fn push_metadata(&mut self, offset: ByteOffset, bytes: Box<[u8]>) -> Result<(), PlanError> {
		let len = ByteLen::from(bytes.len());
		let metadata_bytes = self
			.metadata_bytes
			.0
			.checked_add(len.0)
			.filter(|total| *total <= self.limits.max_metadata_bytes.0)
			.ok_or(PlanError::MetadataLimit)?;
		self.push(offset, len, Payload::Metadata(bytes))?;
		self.metadata_bytes = ByteLen(metadata_bytes);
		Ok(())
	}

	/// Places an exact, bounded slice of an immutable source at `offset`.
	///
	/// # Errors
	/// Rejects invalid source windows and empty, overlapping, gapped, oversized
	/// or over-budget placements.
	pub fn push_source(
		&mut self,
		offset: ByteOffset,
		source: S,
		source_offset: ByteOffset,
		len: ByteLen,
	) -> Result<(), PlanError> {
		let window = Window::new(source, source_offset, len).map_err(PlanError::SourceRange)?;
		self.push(offset, len, Payload::Source(window))
	}

	/// Explicitly assigns zeros to the next `len` bytes without backing storage.
	///
	/// # Errors
	/// Rejects empty, overlapping, gapped, oversized or over-budget placements.
	pub fn push_zero(&mut self, offset: ByteOffset, len: ByteLen) -> Result<(), PlanError> {
		self.push(offset, len, Payload::Zero)
	}

	fn push(
		&mut self,
		offset: ByteOffset,
		len: ByteLen,
		payload: Payload<S>,
	) -> Result<(), PlanError> {
		let range = admit_range(self.end, offset, len, self.image_len)?;
		if self.placements.len() >= self.limits.max_ranges {
			return Err(PlanError::RangeLimit);
		}
		self.placements
			.try_reserve(1)
			.map_err(PlanError::Allocation)?;
		self.placements.push(Placement { range, payload });
		self.end = range.end();
		Ok(())
	}

	/// Consumes the placement plan and makes the complete image readable.
	///
	/// # Errors
	/// Rejects an image whose placements leave an unassigned suffix.
	pub fn seal(self) -> Result<SealedPlan<S>, PlanError> {
		if self.end.0 != self.image_len.0 {
			return Err(PlanError::Incomplete { end: self.end });
		}
		Ok(SealedPlan {
			image_len: self.image_len,
			placements: self.placements,
		})
	}
}

pub(crate) fn admit_range(
	end: ByteOffset,
	offset: ByteOffset,
	len: ByteLen,
	image_len: ByteLen,
) -> Result<ByteRange, PlanError> {
	if offset != end {
		return Err(PlanError::OutOfOrder {
			expected: end,
			actual: offset,
		});
	}
	if len.0 == 0 {
		return Err(PlanError::EmptyRange);
	}
	ByteRange::new(offset, len, image_len).map_err(PlanError::ImageRange)
}

/// An immutable, exact byte layout with no gaps or overlapping placements.
#[derive(Debug)]
pub struct SealedPlan<S> {
	image_len: ByteLen,
	placements: Vec<Placement<S>>,
}

impl<S> SealedPlan<S> {
	/// Returns the complete image length in bytes.
	#[must_use]
	pub const fn image_len(&self) -> ByteLen {
		self.image_len
	}

	/// Iterates over the backing map in ascending image order.
	pub fn ranges(&self) -> impl ExactSizeIterator<Item = (ByteRange, RangeKind)> + '_ {
		self.placements.iter().map(|placement| {
			let kind = match placement.payload {
				Payload::Metadata(_) => RangeKind::Metadata,
				Payload::Source(_) => RangeKind::Source,
				Payload::Zero => RangeKind::Zero,
			};
			(placement.range, kind)
		})
	}
}

impl<S: ReadAt> ReadAt for SealedPlan<S> {
	type Error = ReadError<S::Error>;

	fn len(&self) -> ByteLen {
		self.image_len()
	}

	fn read_exact_at(
		&self,
		offset: ByteOffset,
		mut destination: &mut [u8],
	) -> Result<(), Self::Error> {
		let request = ByteRange::new(offset, destination.len().into(), self.image_len)
			.map_err(ReadError::Bounds)?;
		if request.is_empty() {
			return Ok(());
		}
		let first = self
			.placements
			.partition_point(|entry| entry.range.end() <= offset);
		for entry in &self.placements[first..] {
			let start = entry.range.start().max(offset);
			let end = entry.range.end().min(request.end());
			// Admission bounds each intersection by the usize-sized destination.
			let count = (end.0 - start.0) as usize;
			let (chunk, remaining) = destination.split_at_mut(count);
			let relative = ByteOffset(start.0 - entry.range.start().0);
			match &entry.payload {
				Payload::Metadata(bytes) => {
					// Metadata placement length is derived from the same slice.
					let relative = relative.0 as usize;
					chunk.copy_from_slice(&bytes[relative..relative + count]);
				}
				Payload::Source(source) => {
					source
						.read_exact_at(relative, chunk)
						.map_err(|error| ReadError::Source {
							offset: start,
							error,
						})?
				}
				Payload::Zero => chunk.fill(0),
			}
			destination = remaining;
			if destination.is_empty() {
				break;
			}
		}
		debug_assert!(
			destination.is_empty(),
			"sealed placements cover every admitted byte"
		);
		Ok(())
	}
}

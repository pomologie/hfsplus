//! Exact capacity selection for a fixed HFS+ allocation block size.

use std::{error::Error, fmt};

use hfsplus_core::{ByteLen, ReadAt};
use hfsplus_format::volume::Geometry;

use crate::{SealedPlan, allocation::Allocation};

use super::{VolumeDraft, VolumeError, VolumeOptions, layout};

const SECTOR_BYTES: u64 = 512;

/// How to choose the image length after all generated payloads are bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapacityPolicy {
	/// Choose the minimum capacity with the requested free space.
	Auto {
		/// Minimum unallocated capacity, rounded up to whole allocation blocks.
		free_space: ByteLen,
	},
	/// Choose the minimum capacity subject to a maximum image length.
	ShrinkWithin {
		/// Largest sector-aligned image length permitted by the caller.
		max_bytes: ByteLen,
		/// Minimum unallocated capacity, rounded up to whole allocation blocks.
		free_space: ByteLen,
	},
	/// Keep the requested image length and check that the manifest fits.
	Exact {
		/// Exact sector-aligned image length, including a possible partial block.
		bytes: ByteLen,
		/// Minimum unallocated capacity, rounded up to whole allocation blocks.
		free_space: ByteLen,
	},
}

/// Capacity at the draft's current allocation block size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannedCapacity {
	geometry: Geometry,
	image_bytes: ByteLen,
	minimum_bytes: ByteLen,
	free_bytes: ByteLen,
}

impl PlannedCapacity {
	/// Geometry admitted for the selected policy.
	#[must_use]
	pub const fn geometry(self) -> Geometry {
		self.geometry
	}

	/// Exact raw image length selected by the policy.
	#[must_use]
	pub const fn image_bytes(self) -> ByteLen {
		self.image_bytes
	}

	/// Smallest raw image length at this block size with the requested reserve.
	#[must_use]
	pub const fn minimum_bytes(self) -> ByteLen {
		self.minimum_bytes
	}

	/// Bytes available in whole free allocation blocks after placing the manifest.
	#[must_use]
	pub const fn free_bytes(self) -> ByteLen {
		self.free_bytes
	}
}

/// The requested capacity or draft could not be admitted.
#[derive(Debug)]
#[non_exhaustive]
pub enum CapacityError {
	/// An explicit size must be positive and sector-aligned.
	InvalidSize(ByteLen),
	/// No geometry can hold the footprint and requested reserve at this block size.
	AllocationLimit,
	/// The exact size or maximum is smaller than the required image.
	InsufficientSpace {
		/// Smallest compatible image length at this block size.
		required: ByteLen,
		/// Caller-supplied exact size or maximum.
		available: ByteLen,
	},
	/// The selected geometry's metadata and bitmap exceed the metadata budget.
	MetadataLimit,
	/// The draft failed a filesystem layout check.
	Volume(VolumeError),
}

impl fmt::Display for CapacityError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::InvalidSize(size) => write!(
				f,
				"HFS+ image size {} must be positive and divisible by {SECTOR_BYTES}",
				size.0
			),
			Self::AllocationLimit => {
				f.write_str("HFS+ capacity exceeds the allocation block limit")
			}
			Self::InsufficientSpace {
				required,
				available,
			} => write!(
				f,
				"HFS+ image requires {} bytes with {} available",
				required.0, available.0
			),
			Self::MetadataLimit => f.write_str("HFS+ capacity exceeds the metadata budget"),
			Self::Volume(error) => error.fmt(f),
		}
	}
}

impl Error for CapacityError {
	fn source(&self) -> Option<&(dyn Error + 'static)> {
		match self {
			Self::Volume(error) => Some(error),
			_ => None,
		}
	}
}

impl From<VolumeError> for CapacityError {
	fn from(error: VolumeError) -> Self {
		Self::Volume(error)
	}
}

impl<'label> VolumeOptions<'label> {
	/// Creates options for sizing a draft at the supplied allocation block size.
	/// Use these options with [`VolumeDraft::capacity`] or
	/// [`VolumeDraft::seal_with_capacity`] to choose the image geometry.
	///
	/// # Errors
	/// Rejects an unsupported block size.
	pub fn for_capacity(label: &'label str, block_size: u32) -> Result<Self, CapacityError> {
		let maximum_bytes = u64::from(block_size) * u64::from(u32::MAX);
		Geometry::new(block_size, u32::MAX, maximum_bytes).map_err(VolumeError::from)?;
		let blocks = minimum(block_size, 0)?;
		let bytes = u64::from(block_size) * u64::from(blocks);
		let geometry = Geometry::new(block_size, blocks, bytes).map_err(VolumeError::from)?;
		Ok(Self::new(label, geometry))
	}
}

impl<S: ReadAt> VolumeDraft<'_, S> {
	/// Measures the bound draft and selects a size at its configured block size.
	///
	/// # Errors
	/// Rejects unbound generated payloads, exceeded layout or capacity limits,
	/// and a size outside the selected policy.
	pub fn capacity(&mut self, policy: CapacityPolicy) -> Result<PlannedCapacity, CapacityError> {
		policy.validate_size()?;
		let footprint = layout::measure(self)?;
		select(
			self.options.geometry.block_size(),
			self.options.limits.max_metadata_bytes,
			footprint,
			policy,
		)
	}

	/// Seals one image at the chosen capacity while preserving the draft's IDs.
	///
	/// # Errors
	/// Rejects failed capacity selection or invalid final layout and ranges.
	pub fn seal_with_capacity(
		mut self,
		policy: CapacityPolicy,
	) -> Result<SealedPlan<S>, CapacityError> {
		self.options.geometry = self.capacity(policy)?.geometry();
		Ok(self.seal()?)
	}
}

impl CapacityPolicy {
	fn validate_size(self) -> Result<(), CapacityError> {
		let Some(size) = self.bound() else {
			return Ok(());
		};
		if size.0 == 0 || !size.0.is_multiple_of(SECTOR_BYTES) {
			return Err(CapacityError::InvalidSize(size));
		}
		Ok(())
	}

	const fn bound(self) -> Option<ByteLen> {
		match self {
			Self::Auto { .. } => None,
			Self::ShrinkWithin { max_bytes, .. } => Some(max_bytes),
			Self::Exact { bytes, .. } => Some(bytes),
		}
	}

	const fn free_space(self) -> ByteLen {
		match self {
			Self::Auto { free_space }
			| Self::ShrinkWithin { free_space, .. }
			| Self::Exact { free_space, .. } => free_space,
		}
	}
}

/// A measured layout excluding geometry-dependent header reservations and bitmap.
pub(super) struct Footprint {
	pub(super) allocated_blocks: u32,
	/// Metadata holds tree bytes, symlink targets, and both headers.
	pub(super) metadata_bytes: u64,
}

fn select(
	block_size: u32,
	metadata_limit: ByteLen,
	footprint: Footprint,
	policy: CapacityPolicy,
) -> Result<PlannedCapacity, CapacityError> {
	let block_bytes = u64::from(block_size);
	let required_blocks =
		u64::from(footprint.allocated_blocks) + policy.free_space().0.div_ceil(block_bytes);
	let minimum_blocks = minimum(block_size, required_blocks)?;
	let minimum_bytes = ByteLen(u64::from(minimum_blocks) * block_bytes);
	if let Some(available) = policy.bound().filter(|bound| bound.0 < minimum_bytes.0) {
		return Err(CapacityError::InsufficientSpace {
			required: minimum_bytes,
			available,
		});
	}
	let image_bytes = match policy {
		CapacityPolicy::Exact { bytes, .. } => bytes,
		_ => minimum_bytes,
	};
	let geometry = geometry(block_size, image_bytes).ok_or(CapacityError::AllocationLimit)?;
	let allocation = Allocation::new(geometry).map_err(VolumeError::from)?;
	if footprint
		.metadata_bytes
		.checked_add(allocation.bitmap_bytes())
		.is_none_or(|bytes| bytes > metadata_limit.0)
	{
		return Err(CapacityError::MetadataLimit);
	}
	let free_blocks = allocation
		.free_blocks()
		.checked_sub(footprint.allocated_blocks)
		.ok_or(CapacityError::AllocationLimit)?;
	Ok(PlannedCapacity {
		geometry,
		image_bytes,
		minimum_bytes,
		free_bytes: ByteLen(u64::from(free_blocks) * block_bytes),
	})
}

fn minimum(block_size: u32, required_blocks: u64) -> Result<u32, CapacityError> {
	let fits = |blocks| {
		let bytes = ByteLen(u64::from(blocks) * u64::from(block_size));
		geometry(block_size, bytes)
			.and_then(|geometry| Allocation::new(geometry).ok())
			.is_some_and(|allocation| u64::from(allocation.free_blocks()) >= required_blocks)
	};
	let mut first_fit = u32::MAX;
	if !fits(first_fit) {
		return Err(CapacityError::AllocationLimit);
	}
	let mut too_small = 0;
	// With a fixed block size, reservations are constant except for the bitmap.
	// Each added block can grow the bitmap by at most one block, so usable
	// capacity is monotone. A u32 block count bounds the search to 32 steps.
	while first_fit - too_small > 1 {
		let candidate = too_small.midpoint(first_fit);
		if fits(candidate) {
			first_fit = candidate;
		} else {
			too_small = candidate;
		}
	}
	Ok(first_fit)
}

fn geometry(block_size: u32, bytes: ByteLen) -> Option<Geometry> {
	let blocks = u32::try_from(bytes.0 / u64::from(block_size)).ok()?;
	Geometry::new(block_size, blocks, bytes.0).ok()
}

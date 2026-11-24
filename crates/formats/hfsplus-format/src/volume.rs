//! Volume header, geometry and embedded fork descriptors from Apple TN1150.

use core::ops::Range;

use crate::{
	Error,
	binary::{field, u16_at, u32_at, u64_at},
};

mod encode;

/// Primary header start relative to the raw bounded volume.
pub const PRIMARY_HEADER_OFFSET: u64 = 1024;
/// On-disk length of a volume header.
pub const HEADER_BYTES: usize = 512;
/// Start of the alternate header relative to the volume end.
const ALTERNATE_HEADER_FROM_END: u64 = 1024;
const MIN_VOLUME_BYTES: u64 =
	PRIMARY_HEADER_OFFSET + HEADER_BYTES as u64 + ALTERNATE_HEADER_FROM_END;
const MIN_ALLOCATION_BLOCK_BYTES: u32 = 512;
const HFS_PLUS_SIGNATURE: [u8; 2] = *b"H+";
const HFS_X_SIGNATURE: [u8; 2] = *b"HX";
const HFS_PLUS_VERSION: u16 = 4;
const HFS_X_VERSION: u16 = 5;
const CLEAN_UNMOUNT_ATTRIBUTE: u32 = 1 << 8;
const JOURNALED_ATTRIBUTE: u32 = 1 << 13;
const LAST_MOUNTED_BYTES: usize = 4;
const FINDER_INFO_BYTES: usize = 32;
const VOLUME_DATE_COUNT: usize = 4;
const DEFAULT_CLUMP_SIZE_COUNT: usize = 2;
const EMBEDDED_EXTENT_COUNT: usize = 8;
const EXTENT_BYTES: usize = size_of::<u32>() * 2;
/// On-disk length of one fork descriptor in bytes.
pub const FORK_BYTES: usize = 80;

mod header_offset {
	use super::FORK_BYTES;

	pub(super) const SIGNATURE: usize = 0;
	pub(super) const VERSION: usize = 2;
	pub(super) const ATTRIBUTES: usize = 4;
	pub(super) const LAST_MOUNTED: usize = 8;
	pub(super) const JOURNAL_INFO: usize = 12;
	pub(super) const CREATION_DATE: usize = 16;
	pub(super) const MODIFICATION_DATE: usize = 20;
	pub(super) const BACKUP_DATE: usize = 24;
	pub(super) const CHECKED_DATE: usize = 28;
	pub(super) const FILE_COUNT: usize = 32;
	pub(super) const FOLDER_COUNT: usize = 36;
	pub(super) const ALLOCATION_BLOCK_SIZE: usize = 40;
	pub(super) const TOTAL_BLOCKS: usize = 44;
	pub(super) const FREE_BLOCKS: usize = 48;
	pub(super) const NEXT_ALLOCATION: usize = 52;
	pub(super) const RESOURCE_CLUMP_SIZE: usize = 56;
	pub(super) const DATA_CLUMP_SIZE: usize = 60;
	pub(super) const NEXT_CATALOG_ID: usize = 64;
	pub(super) const WRITE_COUNT: usize = 68;
	pub(super) const ENCODINGS_BITMAP: usize = 72;
	pub(super) const FINDER_INFO: usize = 80;
	pub(super) const ALLOCATION_FILE: usize = FINDER_INFO + super::FINDER_INFO_BYTES;
	pub(super) const EXTENTS_FILE: usize = ALLOCATION_FILE + FORK_BYTES;
	pub(super) const CATALOG_FILE: usize = EXTENTS_FILE + FORK_BYTES;
	pub(super) const ATTRIBUTES_FILE: usize = CATALOG_FILE + FORK_BYTES;
	pub(super) const STARTUP_FILE: usize = ATTRIBUTES_FILE + FORK_BYTES;
}

mod fork_offset {
	pub(super) const LOGICAL_BYTES: usize = 0;
	pub(super) const CLUMP_BYTES: usize = 8;
	pub(super) const TOTAL_BLOCKS: usize = 12;
	pub(super) const EMBEDDED_EXTENTS: usize = 16;
	pub(super) const EXTENT_START_BLOCK: usize = 0;
	pub(super) const EXTENT_BLOCK_COUNT: usize = size_of::<u32>();
}

/// Filesystem identified by the volume signature and its required version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signature {
	/// Classic HFS+ with historical case-folded catalog keys.
	HfsPlus,
	/// HFSX. Read the catalog B-tree comparison type to determine case policy.
	HfsX,
}

/// Index in the volume allocation block namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AllocationBlock(pub u32);

/// ID in the volume catalog namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CatalogId(pub u32);

impl CatalogId {
	/// First catalog ID that can be allocated to a user entry.
	pub const FIRST_USER: Self = Self(16);
}

/// Validated allocation block geometry for the bounded raw device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
	block_size: u32,
	total_blocks: u32,
	device_bytes: u64,
}

impl Geometry {
	/// Admit a power-of-two block size of at least 512 bytes and a nonempty
	/// allocation space large enough for the headers, within the device.
	///
	/// # Errors
	/// Returns [`Error::InvalidGeometry`] when the fields describe a bad volume.
	pub fn new(block_size: u32, total_blocks: u32, device_bytes: u64) -> Result<Self, Error> {
		let bytes = u64::from(block_size) * u64::from(total_blocks);
		if block_size < MIN_ALLOCATION_BLOCK_BYTES
			|| !block_size.is_power_of_two()
			|| bytes < MIN_VOLUME_BYTES
			|| bytes > device_bytes
		{
			return Err(Error::InvalidGeometry);
		}
		Ok(Self {
			block_size,
			total_blocks,
			device_bytes,
		})
	}

	/// Number of bytes in an allocation block.
	#[must_use]
	pub fn block_size(self) -> u32 {
		self.block_size
	}

	/// Number of allocation blocks in the volume.
	#[must_use]
	pub fn total_blocks(self) -> u32 {
		self.total_blocks
	}

	/// Extent of the allocated block address space in bytes, including free blocks.
	#[must_use]
	pub fn allocation_bytes(self) -> u64 {
		u64::from(self.block_size) * u64::from(self.total_blocks)
	}

	/// Offset of the alternate header relative to the bounded device, which may
	/// have trailing bytes after the last allocation block.
	#[must_use]
	pub fn alternate_header_offset(self) -> u64 {
		self.device_bytes - ALTERNATE_HEADER_FROM_END
	}

	/// Convert a block range to a bounded byte window.
	///
	/// # Errors
	/// Returns [`Error::InvalidGeometry`] when the range escapes the allocation space.
	pub fn block_range(self, start: AllocationBlock, count: u32) -> Result<Range<u64>, Error> {
		let end = start
			.0
			.checked_add(count)
			.filter(|&end| end <= self.total_blocks)
			.ok_or(Error::InvalidGeometry)?;
		let block = u64::from(self.block_size);
		Ok(u64::from(start.0) * block..u64::from(end) * block)
	}
}

/// One contiguous run of allocation blocks in a fork.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extent {
	/// First allocated block.
	pub start: AllocationBlock,
	/// Number of allocated blocks.
	pub blocks: u32,
}

const EMPTY_EXTENT: Extent = Extent {
	start: AllocationBlock(0),
	blocks: 0,
};

/// The embedded part of one file fork. Any overflow extents must be resolved by a tree reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fork {
	/// Logical EOF in bytes.
	pub logical_bytes: u64,
	/// Format clump-size hint, in bytes.
	pub clump_bytes: u32,
	/// Total allocated blocks, including possible overflow extents.
	pub total_blocks: u32,
	extents: [Extent; EMBEDDED_EXTENT_COUNT],
	embedded_blocks: u32,
}

impl Fork {
	/// Construct a descriptor with zero logical bytes and zero allocation.
	#[must_use]
	pub const fn empty() -> Self {
		Self {
			logical_bytes: 0,
			clump_bytes: 0,
			total_blocks: 0,
			extents: [EMPTY_EXTENT; EMBEDDED_EXTENT_COUNT],
			embedded_blocks: 0,
		}
	}

	/// Construct a descriptor from its logical size, allocation block count, and
	/// ordered embedded extents. When all eight slots are used, `total_blocks` may
	/// include allocation described by the overflow tree.
	///
	/// # Errors
	/// Returns [`Error::InvalidFork`] if the extent count exceeds the embedded
	/// capacity, an extent overlaps another or escapes the volume, or the
	/// extents, allocation count, and logical size disagree.
	pub fn new(
		logical_bytes: u64,
		clump_bytes: u32,
		total_blocks: u32,
		extents: &[Extent],
		geometry: Geometry,
	) -> Result<Self, Error> {
		let mut fork = Self {
			logical_bytes,
			clump_bytes,
			total_blocks,
			..Self::empty()
		};
		fork.extents
			.get_mut(..extents.len())
			.ok_or(Error::InvalidFork)?
			.copy_from_slice(extents);
		fork.embedded_blocks = fork.validate(geometry)?;
		Ok(fork)
	}

	/// Decode an 80-byte fork descriptor and validate its embedded extents against the volume.
	///
	/// # Errors
	/// Returns an error for a truncated record or an inconsistent/out-of-range extent.
	pub fn decode(bytes: &[u8], geometry: Geometry) -> Result<Self, Error> {
		let bytes = bytes.get(..FORK_BYTES).ok_or(Error::Truncated)?;
		let mut fork = Self {
			logical_bytes: u64_at(bytes, fork_offset::LOGICAL_BYTES)?,
			clump_bytes: u32_at(bytes, fork_offset::CLUMP_BYTES)?,
			total_blocks: u32_at(bytes, fork_offset::TOTAL_BLOCKS)?,
			..Self::empty()
		};
		for (index, extent) in fork.extents.iter_mut().enumerate() {
			let offset = fork_offset::EMBEDDED_EXTENTS + EXTENT_BYTES * index;
			let start = AllocationBlock(u32_at(bytes, offset + fork_offset::EXTENT_START_BLOCK)?);
			let blocks = u32_at(bytes, offset + fork_offset::EXTENT_BLOCK_COUNT)?;
			*extent = Extent { start, blocks };
		}
		fork.embedded_blocks = fork.validate(geometry)?;
		Ok(fork)
	}

	fn validate(&self, geometry: Geometry) -> Result<u32, Error> {
		if self.total_blocks > geometry.total_blocks
			|| self.logical_bytes > u64::from(self.total_blocks) * u64::from(geometry.block_size)
		{
			return Err(Error::InvalidFork);
		}
		let mut embedded_blocks = 0u32;
		let mut exhausted = false;
		for (index, &Extent { start, blocks }) in self.extents.iter().enumerate() {
			if blocks == EMPTY_EXTENT.blocks {
				if start != EMPTY_EXTENT.start {
					return Err(Error::InvalidFork);
				}
				exhausted = true;
				continue;
			}
			if exhausted {
				return Err(Error::InvalidFork);
			}
			let range = geometry
				.block_range(start, blocks)
				.map_err(|_| Error::InvalidFork)?;
			for earlier in &self.extents[..index] {
				let earlier = geometry
					.block_range(earlier.start, earlier.blocks)
					.map_err(|_| Error::InvalidFork)?;
				if range.start < earlier.end && earlier.start < range.end {
					return Err(Error::InvalidFork);
				}
			}
			embedded_blocks = embedded_blocks
				.checked_add(blocks)
				.filter(|&count| count <= self.total_blocks)
				.ok_or(Error::InvalidFork)?;
		}
		if exhausted && embedded_blocks != self.total_blocks {
			return Err(Error::InvalidFork);
		}
		Ok(embedded_blocks)
	}

	/// Embedded nonempty extents in logical fork order.
	pub fn extents(&self) -> impl Iterator<Item = Extent> + '_ {
		self.extents
			.iter()
			.copied()
			.take_while(|extent| extent.blocks != EMPTY_EXTENT.blocks)
	}

	/// Whether additional extents are needed to read the full fork allocation.
	#[must_use]
	pub fn has_overflow(&self) -> bool {
		self.embedded_blocks < self.total_blocks
	}
}

/// The five special file forks embedded in the volume header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpecialFiles {
	/// Allocation bitmap.
	pub allocation: Fork,
	/// Extents overflow B-tree.
	pub extents: Fork,
	/// Catalog B-tree.
	pub catalog: Fork,
	/// Attributes B-tree.
	pub attributes: Fork,
	/// Bootstrapping file, if present.
	pub startup: Fork,
}

/// Parsed primary or alternate HFS+ volume header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Header {
	/// Filesystem identified at this header.
	pub signature: Signature,
	/// Validated allocation block geometry.
	pub geometry: Geometry,
	/// Raw attribute mask, including unrecognized bits.
	pub attributes: u32,
	/// Mounting implementation identifier as a raw FourCC.
	pub last_mounted: [u8; LAST_MOUNTED_BYTES],
	/// Journal info allocation block, if journaled.
	pub journal_info: Option<AllocationBlock>,
	/// Creation, modification, backup and checked timestamps in HFS seconds since 1904.
	pub dates: [u32; VOLUME_DATE_COUNT],
	/// Files recorded in the mounted volume namespace.
	pub files: u32,
	/// Folders recorded in the mounted volume namespace.
	pub folders: u32,
	/// Free allocation blocks declared in the header.
	pub free_blocks: u32,
	/// Next-allocation search hint.
	pub next_allocation: AllocationBlock,
	/// Default resource and data fork clump sizes, in bytes.
	pub clump_bytes: [u32; DEFAULT_CLUMP_SIZE_COUNT],
	/// Next catalog ID declared in the header.
	pub next_catalog_id: CatalogId,
	/// Header write counter.
	pub writes: u32,
	/// Raw text encoding hint bitmap.
	pub encodings: u64,
	/// Opaque Finder fields from the volume header.
	pub finder_info: [u8; FINDER_INFO_BYTES],
	/// The special file forks.
	pub forks: SpecialFiles,
	// Its bytes are reserved on unjournaled volumes and survive a read/write.
	reserved_journal_info: u32,
}

impl Header {
	/// Decode a 512-byte volume header for a caller-supplied bounded raw volume.
	/// This does not walk the allocation bitmap, verify the catalog or replay a journal.
	///
	/// # Errors
	/// Reports an invalid signature/version, truncated header, invalid device geometry or invalid fork.
	pub fn decode(bytes: &[u8], device_bytes: u64) -> Result<Self, Error> {
		let bytes = bytes.get(..HEADER_BYTES).ok_or(Error::Truncated)?;
		let version = u16_at(bytes, header_offset::VERSION)?;
		let signature = match (field(bytes, header_offset::SIGNATURE)?, version) {
			(HFS_PLUS_SIGNATURE, HFS_PLUS_VERSION) => Signature::HfsPlus,
			(HFS_X_SIGNATURE, HFS_X_VERSION) => Signature::HfsX,
			(HFS_PLUS_SIGNATURE | HFS_X_SIGNATURE, _) => {
				return Err(Error::UnsupportedVersion(version));
			}
			_ => return Err(Error::InvalidSignature),
		};
		let geometry = Geometry::new(
			u32_at(bytes, header_offset::ALLOCATION_BLOCK_SIZE)?,
			u32_at(bytes, header_offset::TOTAL_BLOCKS)?,
			device_bytes,
		)?;
		let free_blocks = u32_at(bytes, header_offset::FREE_BLOCKS)?;
		let next_allocation = AllocationBlock(u32_at(bytes, header_offset::NEXT_ALLOCATION)?);
		validate_allocation_counters(geometry, free_blocks, next_allocation)?;
		let attributes = u32_at(bytes, header_offset::ATTRIBUTES)?;
		let journal_block = u32_at(bytes, header_offset::JOURNAL_INFO)?;
		let (journal_info, reserved_journal_info) = if attributes & JOURNALED_ATTRIBUTE != 0 {
			let block = AllocationBlock(journal_block);
			geometry.block_range(block, 1)?;
			(Some(block), 0)
		} else {
			(None, journal_block)
		};
		let fork = |offset| Fork::decode(&bytes[offset..], geometry);
		Ok(Self {
			signature,
			geometry,
			attributes,
			last_mounted: field(bytes, header_offset::LAST_MOUNTED)?,
			journal_info,
			reserved_journal_info,
			dates: [
				u32_at(bytes, header_offset::CREATION_DATE)?,
				u32_at(bytes, header_offset::MODIFICATION_DATE)?,
				u32_at(bytes, header_offset::BACKUP_DATE)?,
				u32_at(bytes, header_offset::CHECKED_DATE)?,
			],
			files: u32_at(bytes, header_offset::FILE_COUNT)?,
			folders: u32_at(bytes, header_offset::FOLDER_COUNT)?,
			free_blocks,
			next_allocation,
			clump_bytes: [
				u32_at(bytes, header_offset::RESOURCE_CLUMP_SIZE)?,
				u32_at(bytes, header_offset::DATA_CLUMP_SIZE)?,
			],
			next_catalog_id: CatalogId(u32_at(bytes, header_offset::NEXT_CATALOG_ID)?),
			writes: u32_at(bytes, header_offset::WRITE_COUNT)?,
			encodings: u64_at(bytes, header_offset::ENCODINGS_BITMAP)?,
			finder_info: field(bytes, header_offset::FINDER_INFO)?,
			forks: SpecialFiles {
				allocation: fork(header_offset::ALLOCATION_FILE)?,
				extents: fork(header_offset::EXTENTS_FILE)?,
				catalog: fork(header_offset::CATALOG_FILE)?,
				attributes: fork(header_offset::ATTRIBUTES_FILE)?,
				startup: fork(header_offset::STARTUP_FILE)?,
			},
		})
	}

	/// Whether the volume declares a clean unmount in its attributes.
	#[must_use]
	pub fn unmounted_cleanly(&self) -> bool {
		self.attributes & CLEAN_UNMOUNT_ATTRIBUTE != 0
	}
}

fn validate_allocation_counters(
	geometry: Geometry,
	free_blocks: u32,
	next_allocation: AllocationBlock,
) -> Result<(), Error> {
	if free_blocks > geometry.total_blocks() || next_allocation.0 > geometry.total_blocks() {
		return Err(Error::InvalidGeometry);
	}
	Ok(())
}

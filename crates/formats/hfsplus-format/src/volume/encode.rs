use crate::{Error, binary::put};

use super::{
	AllocationBlock, CLEAN_UNMOUNT_ATTRIBUTE, CatalogId, DEFAULT_CLUMP_SIZE_COUNT, EXTENT_BYTES,
	FINDER_INFO_BYTES, FORK_BYTES, Fork, Geometry, HEADER_BYTES, HFS_PLUS_SIGNATURE,
	HFS_PLUS_VERSION, HFS_X_SIGNATURE, HFS_X_VERSION, Header, JOURNALED_ATTRIBUTE,
	LAST_MOUNTED_BYTES, Signature, SpecialFiles, VOLUME_DATE_COUNT, fork_offset, header_offset,
	validate_allocation_counters,
};

// Identify this formatter separately from Apple's on-disk writer codes (TN1150).
const FORMATTER_VERSION: [u8; LAST_MOUNTED_BYTES] = *b"PmH1";
const INITIAL_WRITE_COUNT: u32 = 1;

impl Fork {
	/// Encode this fork descriptor in big-endian HFS+ format.
	///
	/// # Errors
	/// Rejects inconsistent sizes and extents relative to `geometry`, including
	/// fields changed since construction or decoding.
	pub fn encode(&self, geometry: Geometry) -> Result<[u8; FORK_BYTES], Error> {
		self.validate(geometry)?;
		let mut bytes = [0; FORK_BYTES];
		put(
			&mut bytes,
			fork_offset::LOGICAL_BYTES,
			self.logical_bytes.to_be_bytes(),
		)?;
		put(
			&mut bytes,
			fork_offset::CLUMP_BYTES,
			self.clump_bytes.to_be_bytes(),
		)?;
		put(
			&mut bytes,
			fork_offset::TOTAL_BLOCKS,
			self.total_blocks.to_be_bytes(),
		)?;
		for (index, extent) in self.extents.iter().enumerate() {
			let offset = fork_offset::EMBEDDED_EXTENTS + EXTENT_BYTES * index;
			put(
				&mut bytes,
				offset + fork_offset::EXTENT_START_BLOCK,
				extent.start.0.to_be_bytes(),
			)?;
			put(
				&mut bytes,
				offset + fork_offset::EXTENT_BLOCK_COUNT,
				extent.blocks.to_be_bytes(),
			)?;
		}
		Ok(bytes)
	}
}

impl Header {
	/// Create a clean, unjournaled HFS+ header for the given format geometry.
	///
	/// The initial counters report no user entries and no free blocks. The
	/// image builder sets the allocation and catalog counters,
	/// timestamps, and Finder fields from its finalized plan before encoding.
	///
	/// # Errors
	/// Rejects fork descriptors inconsistent with the supplied geometry.
	pub fn new_clean(geometry: Geometry, forks: SpecialFiles) -> Result<Self, Error> {
		for fork in [
			forks.allocation,
			forks.extents,
			forks.catalog,
			forks.attributes,
			forks.startup,
		] {
			fork.validate(geometry)?;
		}
		Ok(Self {
			signature: Signature::HfsPlus,
			geometry,
			attributes: CLEAN_UNMOUNT_ATTRIBUTE,
			last_mounted: FORMATTER_VERSION,
			journal_info: None,
			dates: [0; VOLUME_DATE_COUNT],
			files: 0,
			folders: 0,
			free_blocks: 0,
			next_allocation: AllocationBlock(0),
			clump_bytes: [geometry.block_size(); DEFAULT_CLUMP_SIZE_COUNT],
			next_catalog_id: CatalogId::FIRST_USER,
			writes: INITIAL_WRITE_COUNT,
			encodings: 0,
			finder_info: [0; FINDER_INFO_BYTES],
			forks,
			reserved_journal_info: 0,
		})
	}

	/// Encode one volume header, preserving its raw attributes and opaque fields.
	///
	/// # Errors
	/// Rejects inconsistent allocation counters, journal state or fork descriptors.
	pub fn encode(&self) -> Result<[u8; HEADER_BYTES], Error> {
		validate_allocation_counters(self.geometry, self.free_blocks, self.next_allocation)?;
		let journal_block = match (
			self.attributes & JOURNALED_ATTRIBUTE != 0,
			self.journal_info,
		) {
			(true, Some(block)) => {
				self.geometry.block_range(block, 1)?;
				block.0
			}
			(false, None) => self.reserved_journal_info,
			_ => return Err(Error::InvalidGeometry),
		};
		let (signature, version) = match self.signature {
			Signature::HfsPlus => (HFS_PLUS_SIGNATURE, HFS_PLUS_VERSION),
			Signature::HfsX => (HFS_X_SIGNATURE, HFS_X_VERSION),
		};
		let mut bytes = [0; HEADER_BYTES];
		put(&mut bytes, header_offset::SIGNATURE, signature)?;
		put(&mut bytes, header_offset::VERSION, version.to_be_bytes())?;
		put(&mut bytes, header_offset::LAST_MOUNTED, self.last_mounted)?;
		put(&mut bytes, header_offset::FINDER_INFO, self.finder_info)?;
		put(
			&mut bytes,
			header_offset::ENCODINGS_BITMAP,
			self.encodings.to_be_bytes(),
		)?;
		let [creation, modification, backup, checked] = self.dates;
		let [resource_clump, data_clump] = self.clump_bytes;
		for (offset, value) in [
			(header_offset::ATTRIBUTES, self.attributes),
			(header_offset::JOURNAL_INFO, journal_block),
			(header_offset::CREATION_DATE, creation),
			(header_offset::MODIFICATION_DATE, modification),
			(header_offset::BACKUP_DATE, backup),
			(header_offset::CHECKED_DATE, checked),
			(header_offset::FILE_COUNT, self.files),
			(header_offset::FOLDER_COUNT, self.folders),
			(
				header_offset::ALLOCATION_BLOCK_SIZE,
				self.geometry.block_size(),
			),
			(header_offset::TOTAL_BLOCKS, self.geometry.total_blocks()),
			(header_offset::FREE_BLOCKS, self.free_blocks),
			(header_offset::NEXT_ALLOCATION, self.next_allocation.0),
			(header_offset::RESOURCE_CLUMP_SIZE, resource_clump),
			(header_offset::DATA_CLUMP_SIZE, data_clump),
			(header_offset::NEXT_CATALOG_ID, self.next_catalog_id.0),
			(header_offset::WRITE_COUNT, self.writes),
		] {
			put(&mut bytes, offset, value.to_be_bytes())?;
		}
		for (offset, fork) in [
			(header_offset::ALLOCATION_FILE, &self.forks.allocation),
			(header_offset::EXTENTS_FILE, &self.forks.extents),
			(header_offset::CATALOG_FILE, &self.forks.catalog),
			(header_offset::ATTRIBUTES_FILE, &self.forks.attributes),
			(header_offset::STARTUP_FILE, &self.forks.startup),
		] {
			put(&mut bytes, offset, fork.encode(self.geometry)?)?;
		}
		Ok(bytes)
	}
}

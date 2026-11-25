use hfsplus_core::{ByteLen, ByteOffset, ReadAt};
use hfsplus_format::volume::{
	Extent, Fork, Geometry, Header, PRIMARY_HEADER_OFFSET, Signature, SpecialFiles,
};

use crate::{Limits, PlanBuilder, PlanError, SealedPlan};

use super::{
	EntryKind, ForkSource, VolumeError, VolumeFormat,
	catalog::DEFAULT_ENCODING_HINT,
	layout::{self, EntryExtents, Layout, allocated_fork, attributes_clump_bytes},
	manifest::{Input, VolumeDraft},
};

const ALT_HEADER_FROM_END: u64 = 1024;
const DEFAULT_ENCODING_PRESENT: u64 = 1 << DEFAULT_ENCODING_HINT;

pub(super) fn seal<S: ReadAt>(mut draft: VolumeDraft<'_, S>) -> Result<SealedPlan<S>, VolumeError> {
	let mut payloads = Vec::new();
	let Layout {
		allocation,
		catalog,
		extents,
		attributes,
		..
	} = layout::prepare(&mut draft, &mut payloads)?;
	drop(draft.attributes);
	let options = draft.options;
	let geometry = options.geometry;
	let attributes_fork = match &attributes {
		Some(placed) => allocated_fork(
			geometry,
			placed.tree.as_bytes().len() as u64,
			placed.extent,
			attributes_clump_bytes(geometry),
		)?,
		None => Fork::empty(),
	};
	let mut header = Header::new_clean(
		geometry,
		SpecialFiles {
			allocation: allocated_fork(
				geometry,
				allocation.bitmap_file_bytes(),
				allocation.bitmap_extent(),
				geometry.block_size(),
			)?,
			extents: allocated_fork(
				geometry,
				extents.tree.as_bytes().len() as u64,
				extents.extent,
				geometry.block_size(),
			)?,
			catalog: allocated_fork(
				geometry,
				catalog.tree.as_bytes().len() as u64,
				catalog.extent,
				geometry.block_size(),
			)?,
			attributes: attributes_fork,
			startup: Fork::empty(),
		},
	)?;
	header.signature = match options.format {
		VolumeFormat::HfsPlus => Signature::HfsPlus,
		VolumeFormat::HfsX => Signature::HfsX,
	};
	header.files = draft.file_count;
	header.folders = draft.folder_count;
	header.free_blocks = allocation.free_blocks();
	header.next_allocation = allocation.next_allocation();
	header.next_catalog_id = draft.next_id;
	header.dates = options.dates;
	header.finder_info = options.finder_info;
	header.encodings = DEFAULT_ENCODING_PRESENT;
	let header = header.encode()?;

	let mut image = Emitter::new(geometry, options.limits);
	image.metadata(ByteOffset(PRIMARY_HEADER_OFFSET), Box::from(header))?;
	let metadata_limit = usize::try_from(options.limits.max_metadata_bytes.0).unwrap_or(usize::MAX);
	image.extent_metadata(
		allocation.bitmap_extent(),
		allocation.encode_bitmap(metadata_limit)?,
	)?;
	let mut payloads = payloads.into_iter();
	for (entry, placement) in draft.entries.into_iter().zip(payloads.by_ref()) {
		let Input::Ordinary(input) = entry.input else {
			continue;
		};
		match input.kind {
			EntryKind::Folder => {}
			EntryKind::File { data, resource } => {
				image.file(placement, data, resource)?;
			}
			EntryKind::Symlink { target } => {
				let extent = placement
					.data
					.expect("an admitted symbolic link has a target");
				image.extent_metadata(extent, target.into_boxed_slice())?;
			}
			EntryKind::GeneratedFile { .. } => {
				return Err(VolumeError::MissingGeneratedData(entry.id));
			}
		}
	}
	if let Some(links) = draft.hard_links {
		for (file, placement) in links.files.into_iter().zip(payloads) {
			image.file(placement, file.data, file.resource)?;
		}
	}
	image.extent_metadata(catalog.extent, catalog.tree.into_bytes())?;
	image.extent_metadata(extents.extent, extents.tree.into_bytes())?;
	if let Some(placed) = attributes {
		image.extent_metadata(placed.extent, placed.tree.into_bytes())?;
	}
	image.metadata(
		ByteOffset(geometry.alternate_header_offset()),
		Box::from(header),
	)?;
	image.finish()
}

struct Emitter<S> {
	image: PlanBuilder<S>,
	end: ByteOffset,
	image_len: ByteLen,
	geometry: Geometry,
}

impl<S: ReadAt> Emitter<S> {
	fn new(geometry: Geometry, limits: Limits) -> Self {
		let image_len = ByteLen(geometry.alternate_header_offset() + ALT_HEADER_FROM_END);
		Self {
			image: PlanBuilder::new(image_len, limits),
			end: ByteOffset(0),
			image_len,
			geometry,
		}
	}

	fn pad_to(&mut self, offset: ByteOffset) -> Result<(), VolumeError> {
		let padding = offset
			.0
			.checked_sub(self.end.0)
			.ok_or(PlanError::OutOfOrder {
				expected: self.end,
				actual: offset,
			})?;
		if padding != 0 {
			self.image.push_zero(self.end, ByteLen(padding))?;
			self.end = offset;
		}
		Ok(())
	}

	fn extent_start(&self, extent: Extent) -> Result<ByteOffset, VolumeError> {
		Ok(ByteOffset(
			self.geometry
				.block_range(extent.start, extent.blocks)?
				.start,
		))
	}

	fn metadata(&mut self, offset: ByteOffset, bytes: Box<[u8]>) -> Result<(), VolumeError> {
		self.pad_to(offset)?;
		let size = ByteLen::from(bytes.len());
		self.image.push_metadata(offset, bytes)?;
		// Admitting the placement also checks its endpoint.
		self.end = ByteOffset(offset.0 + size.0);
		Ok(())
	}

	fn extent_metadata(&mut self, extent: Extent, bytes: Box<[u8]>) -> Result<(), VolumeError> {
		self.metadata(self.extent_start(extent)?, bytes)
	}

	fn source(&mut self, extent: Extent, fork: ForkSource<S>) -> Result<(), VolumeError> {
		let offset = self.extent_start(extent)?;
		self.pad_to(offset)?;
		self.image
			.push_source(offset, fork.source, fork.range.start(), fork.range.len())?;
		self.end = ByteOffset(offset.0 + fork.range.len().0);
		Ok(())
	}

	fn file(
		&mut self,
		placement: EntryExtents,
		data: ForkSource<S>,
		resource: Option<ForkSource<S>>,
	) -> Result<(), VolumeError> {
		if let Some(extent) = placement.data {
			self.source(extent, data)?;
		}
		if let Some((extent, resource)) = placement.resource.zip(resource) {
			self.source(extent, resource)?;
		}
		Ok(())
	}

	fn finish(mut self) -> Result<SealedPlan<S>, VolumeError> {
		self.pad_to(ByteOffset(self.image_len.0))?;
		Ok(self.image.seal()?)
	}
}

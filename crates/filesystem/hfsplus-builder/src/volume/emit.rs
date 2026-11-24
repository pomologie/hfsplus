use hfsplus_core::{ByteLen, ByteOffset, ReadAt};
use hfsplus_format::{
	btree::{
		BIG_KEYS_MASK, CASE_FOLDING_COMPARE, HFS_BTREE_TYPE, TreeMetadata, VARIABLE_INDEX_KEYS_MASK,
	},
	catalog::{
		EncodeValue, FLAG_THREAD_EXISTS, Key, MAX_KEY_LENGTH, MAX_NAME_UNITS, Metadata,
		Permissions, Record as CatalogRecord, ThreadKind,
	},
	volume::{
		CatalogId, Extent, Fork, Geometry, HEADER_BYTES, Header, PRIMARY_HEADER_OFFSET,
		SpecialFiles,
	},
};

use crate::{
	Limits, PlanBuilder, PlanError, SealedPlan,
	allocation::Allocation,
	tree::{PackedTree, Record as TreeRecord, TreeOptions},
};

use super::{
	EntryKind, EntryMetadata, ForkSource, VolumeError,
	manifest::{AsciiVolumeDraft, ROOT_ID, ROOT_PARENT_ID, basename, compare_names},
};

const NODE_BYTES: u16 = 4096;
const EXTENTS_MAX_KEY_BYTES: u16 = 10;
const EXTENTS_KEY_COMPARE: u8 = 0;
const ALT_HEADER_FROM_END: u64 = 1024;
const FILE_TYPE: u16 = 0o100000;
const FOLDER_TYPE: u16 = 0o040000;
const MAC_ROMAN_ENCODING: u32 = 0;
const MAC_ROMAN_ENCODING_PRESENT: u64 = 1 << MAC_ROMAN_ENCODING;
const RESERVED_CATALOG_FORK_WORD: u32 = 0;
const RECORDS_PER_ENTRY: usize = 2;

pub(super) fn seal<S: ReadAt>(
	draft: AsciiVolumeDraft<'_, S>,
) -> Result<SealedPlan<S>, VolumeError> {
	let AsciiVolumeDraft {
		options,
		entries,
		root_valence,
		folder_count,
		file_count,
		next_id,
	} = draft;
	let geometry = options.geometry;
	let mut allocation = Allocation::new(geometry)?;
	let metadata_limit = usize::try_from(options.limits.max_metadata_bytes.0).unwrap_or(usize::MAX);
	let mut records = Catalog::new(geometry, entries.len(), metadata_limit)?;
	let mut payloads = Vec::new();

	records.add_entry(
		ROOT_PARENT_ID,
		ROOT_ID,
		options.label,
		folder(ROOT_ID, options.root, root_valence),
		ThreadKind::Folder,
	)?;
	for entry in entries {
		let name = basename(entry.input.path);
		let (value, kind) = match entry.input.kind {
			EntryKind::Folder => (
				folder(entry.id, entry.input.metadata, entry.valence),
				ThreadKind::Folder,
			),
			EntryKind::File { data, resource } => {
				let data = plan_fork(data, &mut allocation, &mut payloads, geometry)?;
				let resource = match resource {
					Some(source) => plan_fork(source, &mut allocation, &mut payloads, geometry)?,
					None => Fork::empty(),
				};
				(
					EncodeValue::File {
						metadata: metadata(
							entry.id,
							entry.input.metadata,
							FILE_TYPE,
							FLAG_THREAD_EXISTS,
						),
						data,
						resource,
					},
					ThreadKind::File,
				)
			}
			EntryKind::GeneratedFile { .. } => {
				return Err(VolumeError::MissingGeneratedData(entry.id));
			}
		};
		records.add_entry(entry.parent, entry.id, name, value, kind)?;
	}

	let bitmap_bytes =
		usize::try_from(allocation.bitmap_bytes()).map_err(|_| VolumeError::Limit)?;
	let tree_budget = metadata_limit
		.checked_sub(bitmap_bytes)
		.and_then(|bytes| bytes.checked_sub(HEADER_BYTES * 2))
		.ok_or(VolumeError::Limit)?;
	let extents = PackedTree::build(
		&[],
		tree_options(
			options.max_tree_nodes,
			tree_budget,
			geometry,
			TreeKind::Extents,
		),
	)?;
	let tree_budget = tree_budget
		.checked_sub(extents.as_bytes().len())
		.ok_or(VolumeError::Limit)?;
	let catalog = records.pack(tree_options(
		options.max_tree_nodes,
		tree_budget,
		geometry,
		TreeKind::Catalog,
	))?;
	let catalog_extent = allocate_tree(&mut allocation, &catalog)?;
	let extents_extent = allocate_tree(&mut allocation, &extents)?;
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
				extents.as_bytes().len() as u64,
				extents_extent,
				geometry.block_size(),
			)?,
			catalog: allocated_fork(
				geometry,
				catalog.as_bytes().len() as u64,
				catalog_extent,
				geometry.block_size(),
			)?,
			attributes: Fork::empty(),
			startup: Fork::empty(),
		},
	)?;
	header.files = file_count;
	header.folders = folder_count;
	header.free_blocks = allocation.free_blocks();
	header.next_allocation = allocation.next_allocation();
	header.next_catalog_id = next_id;
	header.dates = options.dates;
	header.finder_info = options.finder_info;
	header.encodings = MAC_ROMAN_ENCODING_PRESENT;
	let header = header.encode()?;

	let mut image = Emitter::new(geometry, options.limits);
	image.metadata(ByteOffset(PRIMARY_HEADER_OFFSET), Box::from(header))?;
	image.extent_metadata(
		allocation.bitmap_extent(),
		allocation.encode_bitmap(metadata_limit)?,
	)?;
	for payload in payloads {
		image.payload(payload)?;
	}
	image.extent_metadata(catalog_extent, catalog.into_bytes())?;
	image.extent_metadata(extents_extent, extents.into_bytes())?;
	image.metadata(
		ByteOffset(geometry.alternate_header_offset()),
		Box::from(header),
	)?;
	image.finish()
}

fn folder(id: CatalogId, input: EntryMetadata, valence: u32) -> EncodeValue<'static> {
	EncodeValue::Folder {
		metadata: metadata(id, input, FOLDER_TYPE, 0),
		valence,
		tail_word: 0,
	}
}

fn metadata(id: CatalogId, input: EntryMetadata, kind: u16, flags: u16) -> Metadata {
	Metadata {
		id,
		flags,
		dates: input.dates,
		permissions: Permissions {
			uid: input.uid,
			gid: input.gid,
			admin_flags: 0,
			owner_flags: 0,
			mode: kind | input.mode,
			special: 0,
		},
		finder_info: input.finder_info,
		encoding: MAC_ROMAN_ENCODING,
	}
}

struct AllocatedSource<S> {
	extent: Extent,
	payload: ForkSource<S>,
}

fn plan_fork<S>(
	payload: ForkSource<S>,
	allocation: &mut Allocation,
	payloads: &mut Vec<AllocatedSource<S>>,
	geometry: Geometry,
) -> Result<Fork, VolumeError> {
	let length = payload.range.len().0;
	let Some(extent) = allocation.allocate(length)? else {
		return Ok(Fork::empty());
	};
	let fork = allocated_fork(geometry, length, extent, RESERVED_CATALOG_FORK_WORD)?;
	payloads.try_reserve(1)?;
	payloads.push(AllocatedSource { extent, payload });
	Ok(fork)
}

fn allocated_fork(
	geometry: Geometry,
	bytes: u64,
	extent: Extent,
	clump: u32,
) -> Result<Fork, VolumeError> {
	Ok(Fork::new(bytes, clump, extent.blocks, &[extent], geometry)?)
}

fn allocate_tree(allocation: &mut Allocation, tree: &PackedTree) -> Result<Extent, VolumeError> {
	Ok(allocation
		.allocate(tree.as_bytes().len() as u64)?
		.expect("a packed tree always has a header node"))
}

enum TreeKind {
	Catalog,
	Extents,
}

fn tree_options(
	max_nodes: u32,
	byte_budget: usize,
	geometry: Geometry,
	kind: TreeKind,
) -> TreeOptions {
	let (max_key_bytes, key_compare_type, attributes) = match kind {
		TreeKind::Catalog => (
			MAX_KEY_LENGTH as u16,
			CASE_FOLDING_COMPARE,
			BIG_KEYS_MASK | VARIABLE_INDEX_KEYS_MASK,
		),
		TreeKind::Extents => (EXTENTS_MAX_KEY_BYTES, EXTENTS_KEY_COMPARE, BIG_KEYS_MASK),
	};
	let budget_nodes = u32::try_from(byte_budget / usize::from(NODE_BYTES)).unwrap_or(u32::MAX);
	TreeOptions {
		node_bytes: NODE_BYTES,
		max_key_bytes,
		max_nodes: max_nodes.min(budget_nodes),
		metadata: TreeMetadata {
			clump_bytes: geometry.block_size(),
			tree_type: HFS_BTREE_TYPE,
			key_compare_type,
			attributes,
		},
	}
}

struct Catalog<'a> {
	geometry: Geometry,
	records: Vec<EncodedRecord<'a>>,
	remaining_record_bytes: usize,
}

struct EncodedRecord<'a> {
	id: CatalogId,
	name: &'a str,
	key_width: usize,
	bytes: Box<[u8]>,
}

impl<'a> Catalog<'a> {
	fn new(geometry: Geometry, entries: usize, metadata_limit: usize) -> Result<Self, VolumeError> {
		let record_count = entries
			.checked_add(1)
			.and_then(|including_root| including_root.checked_mul(RECORDS_PER_ENTRY))
			.ok_or(VolumeError::Limit)?;
		let mut records = Vec::new();
		records.try_reserve_exact(record_count)?;
		Ok(Self {
			geometry,
			records,
			remaining_record_bytes: metadata_limit,
		})
	}

	fn add_entry(
		&mut self,
		parent: CatalogId,
		id: CatalogId,
		name: &'a str,
		value: EncodeValue<'_>,
		kind: ThreadKind,
	) -> Result<(), VolumeError> {
		let mut units = [0; MAX_NAME_UNITS];
		// The admitted name is ASCII, so each byte occupies one UTF-16 unit.
		let encoded_name = &mut units[..name.len()];
		for (unit, byte) in encoded_name.iter_mut().zip(name.bytes()) {
			*unit = u16::from(byte);
		}
		self.push(parent, name, encoded_name, value)?;
		self.push(
			id,
			"",
			&[],
			EncodeValue::Thread {
				kind,
				parent,
				name: encoded_name,
			},
		)
	}

	fn push(
		&mut self,
		id: CatalogId,
		name: &'a str,
		encoded_name: &[u16],
		value: EncodeValue<'_>,
	) -> Result<(), VolumeError> {
		let size = CatalogRecord::encoded_len(encoded_name, value)?;
		self.remaining_record_bytes = self
			.remaining_record_bytes
			.checked_sub(size)
			.ok_or(VolumeError::Limit)?;
		let mut bytes = Vec::new();
		bytes.try_reserve_exact(size)?;
		bytes.resize(size, 0);
		CatalogRecord::encode(id, encoded_name, value, self.geometry, &mut bytes)?;
		self.records.push(EncodedRecord {
			id,
			name,
			key_width: Key::encoded_len(encoded_name)?,
			bytes: bytes.into_boxed_slice(),
		});
		Ok(())
	}

	fn pack(mut self, options: TreeOptions) -> Result<PackedTree, VolumeError> {
		self.records.sort_unstable_by(|left, right| {
			left.id
				.cmp(&right.id)
				.then_with(|| compare_names(left.name, right.name))
		});
		let mut records = Vec::new();
		records.try_reserve_exact(self.records.len())?;
		records.extend(
			self.records
				.iter()
				.map(|entry| TreeRecord::new(&entry.bytes, entry.key_width)),
		);
		Ok(PackedTree::build(&records, options)?)
	}
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

	fn payload(&mut self, allocation: AllocatedSource<S>) -> Result<(), VolumeError> {
		let offset = self.extent_start(allocation.extent)?;
		self.pad_to(offset)?;
		let ForkSource { source, range } = allocation.payload;
		self.image
			.push_source(offset, source, range.start(), range.len())?;
		self.end = ByteOffset(offset.0 + range.len().0);
		Ok(())
	}

	fn finish(mut self) -> Result<SealedPlan<S>, VolumeError> {
		self.pad_to(ByteOffset(self.image_len.0))?;
		Ok(self.image.seal()?)
	}
}

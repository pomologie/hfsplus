use hfsplus_core::ByteLen;
use hfsplus_format::{
	attributes::NATIVE_NODE_BYTES as ATTRIBUTE_NODE_BYTES,
	btree::{
		BIG_KEYS_MASK, CASE_FOLDING_COMPARE, HFS_BTREE_TYPE, TreeMetadata, VARIABLE_INDEX_KEYS_MASK,
	},
	catalog::{MAX_KEY_LENGTH, ThreadKind},
	volume::{Extent, Fork, Geometry, HEADER_BYTES},
};

use crate::{
	allocation::Allocation,
	tree::{PackedTree, TreeOptions},
};

use super::{
	EntryKind, FINDER_TYPE_AND_CREATOR_BYTES, SYMLINK_FINDER_TYPE_AND_CREATOR, VolumeError,
	attributes::Attributes,
	capacity::Footprint,
	catalog::{Catalog, file, folder},
	manifest::{ROOT_ID, ROOT_PARENT_ID, VolumeDraft},
};

const NODE_BYTES: u16 = 4096;
const EXTENTS_MAX_KEY_BYTES: u16 = 10;
const EXTENTS_KEY_COMPARE: u8 = 0;
const FILE_TYPE: u16 = 0o100000;
const SYMLINK_TYPE: u16 = 0o120000;
const RESERVED_CATALOG_FORK_WORD: u32 = 0;

pub(super) struct EntryExtents {
	pub(super) data: Option<Extent>,
	pub(super) resource: Option<Extent>,
}

pub(super) struct PlacedTree {
	pub(super) tree: PackedTree,
	pub(super) extent: Extent,
}

pub(super) struct Layout {
	pub(super) allocation: Allocation,
	pub(super) catalog: PlacedTree,
	pub(super) extents: PlacedTree,
	pub(super) attributes: Option<PlacedTree>,
	metadata_bytes: u64,
}

/// Lays out the manifest at its chosen final geometry.
pub(super) fn prepare<S>(
	draft: &mut VolumeDraft<'_, S>,
	payloads: &mut Vec<EntryExtents>,
) -> Result<Layout, VolumeError> {
	let geometry = draft.options.geometry;
	let allocation = Allocation::new(geometry)?;
	let bitmap_bytes = allocation.bitmap_bytes();
	payloads.try_reserve_exact(draft.entries.len())?;
	place(draft, geometry, allocation, bitmap_bytes, |extents| {
		payloads.push(extents);
	})
}

/// Measures with the largest address space at this block size. Fork bounds
/// and tree shapes are exercised by the same path used for final allocation.
/// The selected geometry's bitmap is charged separately after sizing.
pub(super) fn measure<S>(draft: &mut VolumeDraft<'_, S>) -> Result<Footprint, VolumeError> {
	let block_size = draft.options.geometry.block_size();
	let capacity_bytes = u64::from(block_size) * u64::from(u32::MAX);
	let geometry = Geometry::new(block_size, u32::MAX, capacity_bytes)?;
	let allocation = Allocation::new(geometry)?;
	let before = allocation.free_blocks();
	let placed = place(draft, geometry, allocation, 0, |_| {})?;
	Ok(Footprint {
		allocated_blocks: before - placed.allocation.free_blocks(),
		metadata_bytes: placed.metadata_bytes,
	})
}

fn place<S>(
	draft: &mut VolumeDraft<'_, S>,
	geometry: Geometry,
	mut allocation: Allocation,
	bitmap_bytes: u64,
	mut record_payload: impl FnMut(EntryExtents),
) -> Result<Layout, VolumeError> {
	let options = &draft.options;
	let metadata_limit = usize::try_from(options.limits.max_metadata_bytes.0).unwrap_or(usize::MAX);
	let mut records = Catalog::new(geometry, draft.entries.len(), metadata_limit)?;
	let mut remaining_metadata = metadata_limit;
	draft.attributes.sort()?;

	records.add_entry(
		ROOT_PARENT_ID,
		ROOT_ID,
		draft.label.as_units(),
		folder(
			ROOT_ID,
			options.root,
			draft.root_valence,
			draft.attributes.owner_flags(ROOT_ID),
		),
		ThreadKind::Folder,
	)?;
	for entry in &draft.entries {
		let flags = draft.attributes.owner_flags(entry.id);
		let mut extents = EntryExtents {
			data: None,
			resource: None,
		};
		let (value, kind) = match &entry.input.kind {
			EntryKind::Folder => (
				folder(entry.id, entry.input.metadata, entry.valence, flags),
				ThreadKind::Folder,
			),
			EntryKind::File { data, resource } => {
				let (data, data_extent) =
					allocate_fork(&mut allocation, geometry, data.range.len().0)?;
				extents.data = data_extent;
				let resource = match resource {
					Some(source) => {
						let (resource, extent) =
							allocate_fork(&mut allocation, geometry, source.range.len().0)?;
						extents.resource = extent;
						resource
					}
					None => Fork::empty(),
				};
				(
					file(
						entry.id,
						entry.input.metadata,
						FILE_TYPE,
						data,
						resource,
						flags,
					),
					ThreadKind::File,
				)
			}
			EntryKind::Symlink { target } => {
				remaining_metadata = remaining_metadata
					.checked_sub(target.len())
					.ok_or(VolumeError::Limit)?;
				let (data, extent) =
					allocate_fork(&mut allocation, geometry, ByteLen::from(target.len()).0)?;
				extents.data = extent;
				let mut input = entry.input.metadata;
				input.finder_info[..FINDER_TYPE_AND_CREATOR_BYTES]
					.copy_from_slice(&SYMLINK_FINDER_TYPE_AND_CREATOR);
				(
					file(entry.id, input, SYMLINK_TYPE, data, Fork::empty(), flags),
					ThreadKind::File,
				)
			}
			EntryKind::GeneratedFile { .. } => {
				return Err(VolumeError::MissingGeneratedData(entry.id));
			}
		};
		record_payload(extents);
		records.add_entry(entry.parent, entry.id, entry.path.name(), value, kind)?;
	}

	let bitmap_bytes = usize::try_from(bitmap_bytes).map_err(|_| VolumeError::Limit)?;
	let tree_budget = remaining_metadata
		.checked_sub(bitmap_bytes)
		.and_then(|bytes| bytes.checked_sub(HEADER_BYTES * 2))
		.ok_or(VolumeError::Limit)?;
	let (trees, unused) = pack_trees(
		records,
		&draft.attributes,
		options.max_tree_nodes,
		tree_budget,
		geometry,
	)?;
	let catalog = allocate_tree(&mut allocation, trees.catalog)?;
	let extents = allocate_tree(&mut allocation, trees.extents)?;
	let attributes = trees
		.attributes
		.map(|tree| allocate_tree(&mut allocation, tree))
		.transpose()?;
	Ok(Layout {
		allocation,
		catalog,
		extents,
		attributes,
		metadata_bytes: ByteLen::from(metadata_limit - unused).0,
	})
}

struct MetadataTrees {
	catalog: PackedTree,
	extents: PackedTree,
	attributes: Option<PackedTree>,
}

fn pack_trees(
	records: Catalog,
	attributes: &Attributes,
	max_nodes: u32,
	byte_budget: usize,
	geometry: Geometry,
) -> Result<(MetadataTrees, usize), VolumeError> {
	let extents = PackedTree::build(
		&[],
		tree_options(max_nodes, byte_budget, geometry, TreeKind::Extents),
	)?;
	let after_extents = byte_budget
		.checked_sub(extents.as_bytes().len())
		.ok_or(VolumeError::Limit)?;
	let catalog = records.pack(tree_options(
		max_nodes,
		after_extents,
		geometry,
		TreeKind::Catalog,
	))?;
	let after_catalog = after_extents
		.checked_sub(catalog.as_bytes().len())
		.ok_or(VolumeError::Limit)?;
	let attributes = attributes.pack(max_nodes, after_catalog, attributes_clump_bytes(geometry))?;
	let attribute_bytes = attributes.as_ref().map_or(0, |tree| tree.as_bytes().len());
	let unused = after_catalog
		.checked_sub(attribute_bytes)
		.ok_or(VolumeError::Limit)?;
	Ok((
		MetadataTrees {
			catalog,
			extents,
			attributes,
		},
		unused,
	))
}

pub(super) fn attributes_clump_bytes(geometry: Geometry) -> u32 {
	geometry.block_size().max(u32::from(ATTRIBUTE_NODE_BYTES))
}

fn allocate_fork(
	allocation: &mut Allocation,
	geometry: Geometry,
	bytes: u64,
) -> Result<(Fork, Option<Extent>), VolumeError> {
	let Some(extent) = allocation.allocate(bytes)? else {
		return Ok((Fork::empty(), None));
	};
	let fork = allocated_fork(geometry, bytes, extent, RESERVED_CATALOG_FORK_WORD)?;
	Ok((fork, Some(extent)))
}

pub(super) fn allocated_fork(
	geometry: Geometry,
	bytes: u64,
	extent: Extent,
	clump: u32,
) -> Result<Fork, VolumeError> {
	Ok(Fork::new(bytes, clump, extent.blocks, &[extent], geometry)?)
}

fn allocate_tree(allocation: &mut Allocation, tree: PackedTree) -> Result<PlacedTree, VolumeError> {
	let extent = allocation
		.allocate(tree.as_bytes().len() as u64)?
		.expect("a packed tree always has a header node");
	Ok(PlacedTree { tree, extent })
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

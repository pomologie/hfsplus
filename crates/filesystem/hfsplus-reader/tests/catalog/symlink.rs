//! Symbolic-link profiles and exact target reads on the native catalog fixture.

use std::{cell::Cell, num::NonZeroUsize};

use hfsplus_core::{ByteLen, ByteOffset, RangeError, ReadAt};
use hfsplus_format::{
	bsd,
	btree::{Kind, Node},
	catalog::{EncodeValue, Record, Value},
	symlink::{FINDER_TYPE_AND_CREATOR, MAX_SYMLINK_TARGET_BYTES, TargetError},
	volume::{CatalogId, Fork},
};
use hfsplus_reader::{
	Volume,
	catalog::{CatalogLimits, EntryKind, ROOT_ID, SymlinkError},
	overflow::OverflowLimits,
};

use super::{Layout, NATIVE, NATIVE_NODE_BYTES, NESTED, PAYLOAD, STORED_FILE, layout, offset_of};

#[test]
fn admits_native_symlink_metadata_before_reading_exact_uninterpreted_bytes() {
	let (id, mut image, target_start) = native_symlink(|_| {});
	let mut target = PAYLOAD.to_vec();
	target[..3].copy_from_slice(b"../");
	*target.last_mut().unwrap() = 0xff;
	image[target_start..target_start + target.len()].copy_from_slice(&target);
	let target_read = Cell::new(None);
	let volume = Volume::open(TargetSource {
		image: &image,
		target_start: ByteOffset(target_start as u64),
		target_read: &target_read,
	})
	.unwrap();
	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	let limit = NonZeroUsize::new(target.len() - 1).unwrap();
	assert!(matches!(
		catalog.symlink(id, limit),
		Err(SymlinkError::TargetLimit { bytes, max_bytes })
			if bytes == target.len() as u64 && max_bytes == limit.get()
	));
	assert!(
		catalog
			.symlink(CatalogId(u32::MAX), limit)
			.unwrap()
			.is_none()
	);
	let symlink = catalog
		.symlink(id, NonZeroUsize::new(target.len()).unwrap())
		.unwrap()
		.unwrap();
	assert_eq!(symlink.target_bytes().get(), target.len());
	assert_eq!(target_read.get(), None);
	assert_eq!(symlink.read(OverflowLimits::default()).unwrap(), target);
	assert_eq!(target_read.get(), Some(target.len()));
}

#[test]
fn rejects_wrong_symlink_profiles_and_invalid_target_bytes() {
	let (id, mut image, target_start) = native_symlink(|_| {});
	let native = Volume::open(NATIVE).unwrap();
	let limit = NonZeroUsize::MAX;
	assert!(matches!(
		native
			.open_catalog(CatalogLimits::default())
			.unwrap()
			.symlink(id, limit),
		Err(SymlinkError::UnsupportedProfile)
	));
	let (id, compressed, _) = native_symlink(|record| {
		record.metadata.permissions.owner_flags |= bsd::COMPRESSED;
	});
	let compressed = Volume::open(compressed.as_slice()).unwrap();
	assert!(matches!(
		compressed
			.open_catalog(CatalogLimits::default())
			.unwrap()
			.symlink(id, limit),
		Err(SymlinkError::UnsupportedProfile)
	));
	let oversized_bytes = (MAX_SYMLINK_TARGET_BYTES + 1) as u64;
	let (id, oversized, _) = native_symlink(|record| record.data.logical_bytes = oversized_bytes);
	let oversized = Volume::open(oversized.as_slice()).unwrap();
	assert!(matches!(
		oversized.open_catalog(CatalogLimits::default()).unwrap().symlink(id, limit),
		Err(SymlinkError::TargetLimit { bytes, max_bytes })
			if bytes == oversized_bytes && max_bytes == MAX_SYMLINK_TARGET_BYTES
	));
	let (id, empty, _) = native_symlink(|record| record.data = Fork::empty());
	let empty = Volume::open(empty.as_slice()).unwrap();
	assert!(matches!(
		empty
			.open_catalog(CatalogLimits::default())
			.unwrap()
			.symlink(id, limit),
		Err(SymlinkError::InvalidTarget(TargetError::Empty))
	));
	image[target_start] = 0;
	let invalid = Volume::open(image.as_slice()).unwrap();
	let catalog = invalid.open_catalog(CatalogLimits::default()).unwrap();
	let symlink = catalog.symlink(id, limit).unwrap().unwrap();
	assert!(matches!(
		symlink.read(OverflowLimits::default()),
		Err(SymlinkError::InvalidTarget(TargetError::Nul))
	));
}

struct LinkRecord {
	metadata: hfsplus_format::catalog::Metadata,
	data: Fork,
}

// Re-encode the native fixture's existing file as a link, retaining its
// thread, name, geometry, and physical data location.
fn native_symlink(edit: impl FnOnce(&mut LinkRecord)) -> (CatalogId, Vec<u8>, usize) {
	let volume = Volume::open(NATIVE).unwrap();
	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	let folder = catalog.lookup(ROOT_ID, NESTED).unwrap().unwrap();
	let file = catalog
		.lookup(folder.metadata.id, STORED_FILE)
		.unwrap()
		.unwrap();
	let EntryKind::File { data, resource } = file.kind else {
		panic!("native file record must have forks");
	};
	let layout = layout(NATIVE);
	let geometry = layout.volume.geometry;
	let original = native_file_record(&layout, file.metadata.id);
	let start = offset_of(NATIVE, original);
	let extent = data.extents().next().unwrap();
	let target_start = geometry
		.block_range(extent.start, extent.blocks)
		.unwrap()
		.start as usize;
	let mut metadata = file.metadata;
	metadata.permissions.mode = bsd::SYMLINK;
	metadata.finder_info[..FINDER_TYPE_AND_CREATOR.len()].copy_from_slice(&FINDER_TYPE_AND_CREATOR);
	let mut record = LinkRecord { metadata, data };
	edit(&mut record);
	let mut image = NATIVE.to_vec();
	let written = Record::encode(
		file.parent,
		&file.name,
		EncodeValue::File {
			metadata: record.metadata,
			data: record.data,
			resource,
		},
		geometry,
		&mut image[start..start + original.len()],
	)
	.unwrap();
	assert_eq!(written, original.len());
	(file.metadata.id, image, target_start)
}

fn native_file_record(layout: &Layout, id: CatalogId) -> &'static [u8] {
	(0..layout.header.total_nodes as usize)
		.map(|node| layout.base + node * NATIVE_NODE_BYTES)
		.map(|start| Node::decode(&NATIVE[start..start + NATIVE_NODE_BYTES]).unwrap())
		.filter(|node| node.kind() == Kind::Leaf)
		.find_map(|node| {
			node.records().find(|raw| {
				let record = Record::decode(raw, layout.volume.geometry).unwrap();
				matches!(record.value, Value::File { metadata, .. } if metadata.id == id)
			})
		})
		.unwrap()
}

struct TargetSource<'a> {
	image: &'a [u8],
	target_start: ByteOffset,
	target_read: &'a Cell<Option<usize>>,
}

impl ReadAt for TargetSource<'_> {
	type Error = RangeError;

	fn len(&self) -> ByteLen {
		ReadAt::len(self.image)
	}

	fn read_exact_at(&self, offset: ByteOffset, bytes: &mut [u8]) -> Result<(), Self::Error> {
		if offset == self.target_start {
			assert_eq!(self.target_read.replace(Some(bytes.len())), None);
		}
		self.image.read_exact_at(offset, bytes)
	}
}

//! Whole-volume witness for a catalog that needs a continuation map node.

use std::{env, fs::File};

use hfsplus_builder::{
	Limits, SealedPlan,
	volume::{EntryKind, EntryMetadata, ForkSource, ManifestEntry, VolumeOptions, plan_ascii},
};
use hfsplus_core::{ByteLen, ByteOffset, ReadAt};
use hfsplus_format::{
	attributes::NATIVE_INLINE_MAX_VALUE_BYTES,
	btree::{Kind, Node, NodeId, TreeHeader},
	volume::{Geometry, HEADER_BYTES, Header, PRIMARY_HEADER_OFFSET},
};

const FILES: u32 = 96_000;
const CATALOG: &str = "Catalog";
const PAYLOAD: &str = "Payload.bin";
const ATTRIBUTE: &str = "org.pomologie.packed-catalog";
const NAME_TAIL_BYTES: usize = 249;
const BLOCK_BYTES: u32 = 4096;
const NODE_BYTES: usize = 4096;
const IMAGE_BYTES: u64 = 256 * 1024 * 1024;
const METADATA_BYTES: ByteLen = ByteLen(512 * 1024 * 1024);
const NODE_LIMIT: u32 = 65_536;
const DATA: &[u8] = &[0xd1; 257 * 1024 + 1];
const RESOURCE: &[u8] = &[0x52; 129 * 1024 + 1];
const VALUE: &[u8] = &[0xa5; NATIVE_INLINE_MAX_VALUE_BYTES];
const RECORDS_PER_ENTRY: u32 = 2;
const ROOT_FOLDER_AND_PAYLOAD: u32 = 3;
const FIRST_MAP_BIT: u8 = 0x80;

#[test]
#[ignore = "produces a 256 MiB native-oracle fixture"]
fn emit_packed_catalog_volume() {
	let output = env::var_os("POMOLOGIE_PACKED_CATALOG_OUTPUT").unwrap();
	let names: Vec<_> = (0..FILES)
		.map(|ordinal| format!("{CATALOG}/{ordinal:05}-{}", "x".repeat(NAME_TAIL_BYTES)))
		.collect();
	let mut entries: Vec<_> = names.iter().map(|name| file(name, &[], None)).collect();
	entries.push(ManifestEntry {
		path: CATALOG,
		metadata: EntryMetadata::new(0o755),
		kind: EntryKind::Folder,
	});
	entries.push(file(PAYLOAD, DATA, Some(RESOURCE)));
	let blocks = u32::try_from(IMAGE_BYTES / u64::from(BLOCK_BYTES)).unwrap();
	let geometry = Geometry::new(BLOCK_BYTES, blocks, IMAGE_BYTES).unwrap();
	let mut options = VolumeOptions::new("Packed Catalog", geometry);
	options.max_tree_nodes = NODE_LIMIT;
	options.limits = Limits {
		max_metadata_bytes: METADATA_BYTES,
		..Limits::default()
	};
	let mut draft = plan_ascii(options, entries).unwrap();
	draft
		.add_inline_attribute(PAYLOAD, ATTRIBUTE, VALUE)
		.unwrap();
	let image = draft.seal().unwrap();
	check_catalog(&image);
	image
		.write_to(&mut File::create_new(output).unwrap())
		.unwrap();
}

fn file<'a>(
	path: &'a str,
	data: &'static [u8],
	resource: Option<&'static [u8]>,
) -> ManifestEntry<'a, &'static [u8]> {
	ManifestEntry {
		path,
		metadata: EntryMetadata::new(0o644),
		kind: EntryKind::File {
			data: ForkSource::whole(data),
			resource: resource.map(ForkSource::whole),
		},
	}
}

fn check_catalog(image: &SealedPlan<&[u8]>) {
	let mut raw_header = [0; HEADER_BYTES];
	image
		.read_exact_at(ByteOffset(PRIMARY_HEADER_OFFSET), &mut raw_header)
		.unwrap();
	let volume = Header::decode(&raw_header, IMAGE_BYTES).unwrap();
	assert_eq!((volume.files, volume.folders), (FILES + 1, 1));
	let catalog = volume.forks.catalog;
	let extent = catalog.extents().next().unwrap();
	assert_eq!(catalog.extents().count(), 1);
	let start = volume
		.geometry
		.block_range(extent.start, extent.blocks)
		.unwrap()
		.start;
	let mut header_bytes = [0; NODE_BYTES];
	let header = node(image, start, NodeId(0), &mut header_bytes);
	let tree = TreeHeader::decode(&header, catalog.logical_bytes).unwrap();
	let header_map = header.record(2).unwrap();
	assert!(tree.total_nodes as usize > header_map.len() * u8::BITS as usize);
	assert!(tree.depth > 2);
	assert_eq!(tree.free_nodes, 0);
	assert_eq!(
		tree.leaf_records,
		(FILES + ROOT_FOLDER_AND_PAYLOAD) * RECORDS_PER_ENTRY
	);
	let mut found = vec![false; tree.total_nodes as usize];
	found[0] = true;
	let mut buffer = [0; NODE_BYTES];
	let mut map = Vec::from(header_map);
	let mut next = header.forward();
	while let Some(id) = next {
		assert!(!std::mem::replace(&mut found[id.0 as usize], true));
		let continuation = node(image, start, id, &mut buffer);
		assert_eq!(continuation.kind(), Kind::Map);
		map.extend_from_slice(continuation.record(0).unwrap());
		next = continuation.forward();
	}
	assert!(map.len() * u8::BITS as usize >= found.len());
	for (ordinal, byte) in map.into_iter().enumerate() {
		for bit in 0..u8::BITS {
			let id = ordinal * u8::BITS as usize + bit as usize;
			assert_eq!(byte & (FIRST_MAP_BIT >> bit) != 0, id < found.len());
		}
	}
	let mut pending = vec![(tree.root.unwrap(), tree.depth)];
	let mut records = 0;
	let mut leaves = 0;
	while let Some((id, height)) = pending.pop() {
		assert!(!std::mem::replace(&mut found[id.0 as usize], true));
		let current = node(image, start, id, &mut buffer);
		assert_eq!(u16::from(current.height()), height);
		if height == 1 {
			assert_eq!(current.kind(), Kind::Leaf);
			records += current.records().count();
			leaves += 1;
		} else {
			assert_eq!(current.kind(), Kind::Index);
			pending.extend(current.records().map(|record| {
				(
					NodeId(u32::from_be_bytes(*record.last_chunk().unwrap())),
					height - 1,
				)
			}));
		}
	}
	assert!(found.into_iter().all(|exists| exists));
	assert_eq!(records, tree.leaf_records as usize);
	println!(
		"Catalog depth {} with {records} records, {leaves} leaves, and {} allocated nodes",
		tree.depth, tree.total_nodes
	);
}

fn node<'a>(
	image: &SealedPlan<&[u8]>,
	start: u64,
	id: NodeId,
	buffer: &'a mut [u8; NODE_BYTES],
) -> Node<'a> {
	let offset = start + u64::from(id.0) * NODE_BYTES as u64;
	image.read_exact_at(ByteOffset(offset), buffer).unwrap();
	Node::decode(buffer).unwrap()
}

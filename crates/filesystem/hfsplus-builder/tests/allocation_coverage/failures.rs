use std::collections::BTreeSet;

use hfsplus_format::{
	btree::{Node, TreeHeader},
	catalog::{Key, Record, Value},
	volume::{FORK_BYTES, Fork, HEADER_BYTES, Header, PRIMARY_HEADER_OFFSET},
};

use super::{BITS_PER_BYTE, BLOCK_BYTES, FIRST_BITMAP_BIT};

const CATALOG_DATA_FORK_OFFSET: usize = 88;

pub(super) fn fixtures(raw: &[u8]) -> [(&'static str, Vec<u8>); 2] {
	let primary = usize::try_from(PRIMARY_HEADER_OFFSET).unwrap();
	let mut header = Header::decode(
		&raw[primary..primary + HEADER_BYTES],
		u64::try_from(raw.len()).unwrap(),
	)
	.unwrap();
	let [(first_at, data), (second_at, empty)] = file_data_forks(raw, header);
	let data_blocks = occupied_by(data);
	assert_eq!(data_blocks.len(), 2);
	assert!(occupied_by(empty).is_empty());
	assert_eq!(counted_free(raw, header), header.free_blocks);

	let mut overlap = raw.to_vec();
	overlap.copy_within(first_at..first_at + FORK_BYTES, second_at);
	let [(_, first), (_, second)] = file_data_forks(&overlap, header);
	assert_eq!(
		occupied_by(first)
			.intersection(&occupied_by(second))
			.copied()
			.collect::<BTreeSet<_>>(),
		data_blocks
	);
	assert_eq!(counted_free(&overlap, header), header.free_blocks);

	let mut free_counter = raw.to_vec();
	header.free_blocks += 1;
	let alternate = usize::try_from(header.geometry.alternate_header_offset()).unwrap();
	for at in [primary, alternate] {
		free_counter[at..at + HEADER_BYTES].copy_from_slice(&header.encode().unwrap());
	}
	assert_eq!(counted_free(&free_counter, header) + 1, header.free_blocks);
	let [(_, first), (_, second)] = file_data_forks(&free_counter, header);
	assert!(occupied_by(first).is_disjoint(&occupied_by(second)));
	[("overlap", overlap), ("free-counter", free_counter)]
}

fn occupied_by(fork: Fork) -> BTreeSet<u32> {
	fork.extents()
		.flat_map(|extent| extent.start.0..extent.start.0 + extent.blocks)
		.collect()
}

fn counted_free(raw: &[u8], header: Header) -> u32 {
	let bitmap = header.forks.allocation.extents().next().unwrap();
	let bitmap_at = usize::try_from(u64::from(bitmap.start.0) * BLOCK_BYTES).unwrap();
	let total = usize::try_from(header.geometry.total_blocks()).unwrap();
	u32::try_from(
		(0..total)
			.filter(|block| {
				raw[bitmap_at + block / BITS_PER_BYTE]
					& (FIRST_BITMAP_BIT >> (block % BITS_PER_BYTE))
					== 0
			})
			.count(),
	)
	.unwrap()
}

fn file_data_forks(raw: &[u8], header: Header) -> [(usize, Fork); 2] {
	let catalog = header.forks.catalog;
	let extent = catalog.extents().next().unwrap();
	let start = usize::try_from(u64::from(extent.start.0) * BLOCK_BYTES).unwrap();
	let node_bytes = usize::from(TreeHeader::node_size_hint(&raw[start..]).unwrap());
	let tree = TreeHeader::decode(
		&Node::decode(&raw[start..start + node_bytes]).unwrap(),
		catalog.logical_bytes,
	)
	.unwrap();
	assert_eq!(tree.first_leaf, tree.last_leaf);
	let start = start + usize::try_from(tree.first_leaf.unwrap().0).unwrap() * node_bytes;
	let leaf = Node::decode(&raw[start..start + node_bytes]).unwrap();
	let mut files = leaf.records().filter_map(|record| {
		let entry = Record::decode(record, header.geometry).unwrap();
		match entry.value {
			Value::File { data, .. } => {
				let (_, value) = Key::split_leaf(record).unwrap();
				let at = value.as_ptr() as usize - raw.as_ptr() as usize;
				Some((at + CATALOG_DATA_FORK_OFFSET, data))
			}
			_ => None,
		}
	});
	let forks = [files.next().unwrap(), files.next().unwrap()];
	assert!(files.next().is_none());
	forks
}

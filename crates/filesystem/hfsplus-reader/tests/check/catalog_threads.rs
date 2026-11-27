use hfsplus_core::{ByteLen, ByteOffset, RangeError, ReadAt};
use hfsplus_format::{
	btree::NodeLinks,
	catalog::{EncodeValue, ThreadKind},
};
use hfsplus_reader::catalog::ROOT_ID;

use super::{
	CatalogProblem, CheckError, CheckLimits, NODE_BYTES, Node, OPTICAL, Record, TreeHeader, Value,
	Volume, optical_file,
};

#[test]
fn identifies_entry_with_missing_or_inconsistent_catalog_thread() {
	let original = Volume::open(OPTICAL).unwrap();
	original.check_structure(CheckLimits::default()).unwrap();
	let (header, _, id, _) = optical_file();
	let extent = header.forks.catalog.extents().next().unwrap();
	let base = header
		.geometry
		.block_range(extent.start, extent.blocks)
		.unwrap()
		.start as usize;
	let root = Node::decode(&OPTICAL[base..base + NODE_BYTES]).unwrap();
	let tree = TreeHeader::decode(&root, header.forks.catalog.logical_bytes).unwrap();
	let node = tree.first_leaf.unwrap();
	assert_eq!(tree.last_leaf, Some(node));
	let start = base + node.0 as usize * NODE_BYTES;
	let leaf = Node::decode(&OPTICAL[start..start + NODE_BYTES]).unwrap();
	let mut entry_ordinal = None;
	let mut thread_ordinal = None;
	let mut root_parent = None;
	let mut name = Vec::new();
	for (ordinal, raw) in leaf.records().enumerate() {
		let record = Record::decode(raw, header.geometry).unwrap();
		match record.value {
			Value::File { metadata, .. } if metadata.id == id => entry_ordinal = Some(ordinal),
			Value::Thread { parent, .. } if record.key.parent_or_thread == ROOT_ID => {
				root_parent = Some(parent);
			}
			Value::Thread {
				parent,
				name: stored,
				kind,
			} if record.key.parent_or_thread == id => {
				assert_eq!((parent, kind), (ROOT_ID, ThreadKind::File));
				thread_ordinal = Some(ordinal);
				name.extend(stored.units());
			}
			_ => {}
		}
	}
	let entry_ordinal = entry_ordinal.unwrap();
	let thread_ordinal = thread_ordinal.unwrap();
	let wrong_parent = EncodeValue::Thread {
		kind: ThreadKind::File,
		parent: root_parent.unwrap(),
		name: &name,
	};
	let mut wrong_thread = vec![0; Record::encoded_len(&[], wrong_parent).unwrap()];
	Record::encode(id, &[], wrong_parent, header.geometry, &mut wrong_thread).unwrap();
	for replacement in [None, Some(wrong_thread.as_slice())] {
		let records: Vec<_> = leaf
			.records()
			.enumerate()
			.filter_map(|(ordinal, record)| {
				if ordinal == thread_ordinal {
					replacement
				} else {
					Some(record)
				}
			})
			.collect();
		let mut projected = [0; NODE_BYTES];
		Node::encode(
			&mut projected,
			leaf.kind(),
			leaf.height(),
			NodeLinks::default(),
			&records,
		)
		.unwrap();
		let volume = Volume::open(LeafView {
			start,
			bytes: &projected,
		})
		.unwrap();
		assert!(matches!(volume.check_structure(CheckLimits::default()),
			Err(CheckError::Structure(CatalogProblem::ThreadRecord { id: actual, node: leaf_id, ordinal }))
				if (actual, leaf_id, ordinal) == (id, node, entry_ordinal)
		));
	}
}

struct LeafView<'a> {
	start: usize,
	bytes: &'a [u8; NODE_BYTES],
}

impl ReadAt for LeafView<'_> {
	type Error = RangeError;

	fn len(&self) -> ByteLen {
		ReadAt::len(OPTICAL)
	}

	fn read_exact_at(&self, offset: ByteOffset, output: &mut [u8]) -> Result<(), Self::Error> {
		OPTICAL.read_exact_at(offset, output)?;
		let request = offset.0 as usize;
		let start = self.start.max(request);
		let end = (self.start + self.bytes.len()).min(request + output.len());
		if start < end {
			output[start - request..end - request]
				.copy_from_slice(&self.bytes[start - self.start..end - self.start]);
		}
		Ok(())
	}
}

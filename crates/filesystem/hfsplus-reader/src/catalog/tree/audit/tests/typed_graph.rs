use core::cell::RefCell;

use hfsplus_core::{ByteLen, ByteOffset, RangeError, ReadAt, names::NameCollation};
use hfsplus_format::{
	btree::{
		Kind, LEAF_HEIGHT, MIN_INDEX_HEIGHT, Node, NodeLinks, TreeHeader, TreeLayout, TreeMetadata,
		TreeTopology,
	},
	catalog::Key,
	volume::CatalogId,
};

use super::{CatalogError, Corruption, NodeId};
use crate::{
	Error,
	alloc::{vec, vec::Vec},
	catalog::{
		Limit,
		tree::{Query, Tree},
	},
};

const NODE_BYTES: usize = 4096;
const ROOT: NodeId = NodeId(1);
const FIRST_LEAF: NodeId = NodeId(2);
const LAST_LEAF: NodeId = NodeId(3);
const NODE_COUNT: u32 = LAST_LEAF.0 + 1;
const KEY_PARENT: CatalogId = CatalogId(2);
const KEY_NAMES: [u16; 2] = [b'a' as u16, b'b' as u16];
const CASE_VARIANTS: [u16; 2] = [b'A' as u16, b'a' as u16];

struct GraphFork {
	nodes: Vec<(NodeId, Vec<u8>)>,
	reads: RefCell<Vec<NodeId>>,
}

impl ReadAt for GraphFork {
	type Error = Error<RangeError>;

	fn len(&self) -> ByteLen {
		ByteLen(u64::from(NODE_COUNT) * NODE_BYTES as u64)
	}

	fn read_exact_at(&self, offset: ByteOffset, destination: &mut [u8]) -> Result<(), Self::Error> {
		let index = offset.0 / NODE_BYTES as u64;
		assert_eq!(offset.0, index * NODE_BYTES as u64);
		assert_eq!(destination.len(), NODE_BYTES);
		let id = NodeId(u32::try_from(index).unwrap());
		self.reads.borrow_mut().push(id);
		let (_, node) = self
			.nodes
			.iter()
			.find(|(node, _)| *node == id)
			.expect("audit only reads scheduled graph nodes");
		destination.copy_from_slice(node);
		Ok(())
	}
}

fn typed_index(names: [u16; 2], children: [NodeId; 2]) -> Vec<u8> {
	let records = names
		.into_iter()
		.zip(children)
		.map(|(name, child)| {
			let names = [name];
			let key_size = Key::encoded_len(&names).unwrap();
			let mut record = vec![0; key_size + size_of::<u32>()];
			Key::encode(KEY_PARENT, &names, &mut record).unwrap();
			record[key_size..].copy_from_slice(&child.0.to_be_bytes());
			record
		})
		.collect::<Vec<_>>();
	let mut node = vec![0; NODE_BYTES];
	Node::encode(
		&mut node,
		Kind::Index,
		MIN_INDEX_HEIGHT,
		NodeLinks::default(),
		&[&records[0], &records[1]],
	)
	.unwrap();
	node
}

fn graph(
	root: Vec<u8>,
	other_nodes: Vec<(NodeId, Vec<u8>)>,
	collation: NameCollation,
) -> Tree<GraphFork> {
	let header = TreeHeader::new(
		TreeLayout {
			node_bytes: u16::try_from(NODE_BYTES).unwrap(),
			max_key_bytes: 10,
			total_nodes: NODE_COUNT,
			free_nodes: 0,
		},
		TreeTopology {
			depth: u16::from(MIN_INDEX_HEIGHT),
			root: Some(ROOT),
			leaf_records: 2,
			first_leaf: Some(FIRST_LEAF),
			last_leaf: Some(LAST_LEAF),
		},
		TreeMetadata::default(),
	)
	.unwrap();
	let mut nodes = vec![(ROOT, root)];
	nodes.extend(other_nodes);
	let allocated = u8::MAX << (u8::BITS - NODE_COUNT);
	Tree {
		fork: GraphFork {
			nodes,
			reads: RefCell::default(),
		},
		header,
		collation,
		map: vec![allocated],
		map_nodes: 1,
	}
}

#[test]
fn audit_rejects_repeated_child_and_ancestor_before_any_child_read() {
	for (children, repeated) in [
		([FIRST_LEAF, FIRST_LEAF], FIRST_LEAF),
		([ROOT, LAST_LEAF], ROOT),
	] {
		let tree = graph(
			typed_index(KEY_NAMES, children),
			Vec::new(),
			NameCollation::CaseFolded,
		);
		let mut query = Query::new::<RangeError>(NODE_BYTES, NODE_COUNT).unwrap();
		assert!(matches!(
			tree.audit_index_graph(&mut query),
			Err(CatalogError::Corrupt { node, reason: Corruption::IndexRoute }) if node == repeated
		));
		assert_eq!(*tree.fork.reads.borrow(), [ROOT]);
		assert_eq!(query.remaining, NODE_COUNT - 1);
	}
}

#[test]
fn audit_checks_child_height_only_after_admitting_its_read() {
	for read_budget in [1, 2] {
		let tree = graph(
			typed_index(KEY_NAMES, [FIRST_LEAF, LAST_LEAF]),
			vec![(FIRST_LEAF, typed_index(KEY_NAMES, [FIRST_LEAF, LAST_LEAF]))],
			NameCollation::CaseFolded,
		);
		let mut query = Query::new::<RangeError>(NODE_BYTES, read_budget).unwrap();
		let result = tree.audit_index_graph(&mut query);
		if read_budget == 1 {
			assert!(matches!(result, Err(CatalogError::Limit(Limit::Nodes))));
			assert_eq!(*tree.fork.reads.borrow(), [ROOT]);
		} else {
			assert!(matches!(
				result,
				Err(CatalogError::Corrupt {
					node: FIRST_LEAF,
					reason: Corruption::NodeKind
				})
			));
			assert_eq!(*tree.fork.reads.borrow(), [ROOT, FIRST_LEAF]);
		}
		assert_eq!(query.remaining, 0);
		assert_eq!(tree.header.depth - 1, u16::from(LEAF_HEIGHT));
	}
}

#[test]
fn audit_rejects_equal_separators_under_the_selected_collation() {
	let duplicate_names = [KEY_NAMES[0]; KEY_NAMES.len()];
	for (names, collation) in [
		(duplicate_names, NameCollation::Binary),
		(duplicate_names, NameCollation::CaseFolded),
		(CASE_VARIANTS, NameCollation::CaseFolded),
	] {
		let tree = graph(
			typed_index(names, [FIRST_LEAF, LAST_LEAF]),
			Vec::new(),
			collation,
		);
		let mut query = Query::new::<RangeError>(NODE_BYTES, NODE_COUNT).unwrap();
		assert!(
			matches!(
				tree.audit_index_graph(&mut query),
				Err(CatalogError::Corrupt {
					node: ROOT,
					reason: Corruption::RecordOrder
				}),
			),
			"{names:?} {collation:?}"
		);
		assert_eq!(*tree.fork.reads.borrow(), [ROOT]);
		assert_eq!(query.remaining, NODE_COUNT - 1);
	}

	// Binary ordering admits the two distinct spellings. The one-read budget
	// stops the audit at its first child instead of reading either child.
	let tree = graph(
		typed_index(CASE_VARIANTS, [FIRST_LEAF, LAST_LEAF]),
		Vec::new(),
		NameCollation::Binary,
	);
	let mut query = Query::new::<RangeError>(NODE_BYTES, 1).unwrap();
	assert!(matches!(
		tree.audit_index_graph(&mut query),
		Err(CatalogError::Limit(Limit::Nodes))
	));
	assert_eq!(*tree.fork.reads.borrow(), [ROOT]);
	assert_eq!(query.remaining, 0);
}

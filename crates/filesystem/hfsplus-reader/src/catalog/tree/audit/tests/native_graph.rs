use core::cell::RefCell;

use hfsplus_core::{ByteLen, ByteOffset, RangeError, ReadAt};

use crate::{Volume, alloc::vec::Vec, catalog::CatalogLimits};

use super::NodeId;

const NATIVE: &[u8] = include_bytes!("../../../../../tests/fixtures/native-catalog-volume.bin");
const CATALOG_START: u64 = 2_048;
const NODE_BYTES: usize = 4_096;
const ROOT: NodeId = NodeId(3);
// Extracted independently from the native nodes' forward links in leaf order.
const LEAVES: [u32; 16] = [1, 8, 13, 15, 10, 9, 6, 2, 12, 17, 16, 4, 11, 14, 7, 5];

#[derive(Default)]
struct ReadTrace(RefCell<Vec<(ByteOffset, usize)>>);

impl ReadAt for ReadTrace {
	type Error = RangeError;

	fn len(&self) -> ByteLen {
		NATIVE.len().into()
	}

	fn read_exact_at(&self, offset: ByteOffset, destination: &mut [u8]) -> Result<(), Self::Error> {
		self.0.borrow_mut().push((offset, destination.len()));
		NATIVE.read_exact_at(offset, destination)
	}
}

#[test]
fn audits_the_native_optical_graph_in_leaf_order_at_exact_read_budget() {
	let source = ReadTrace::default();
	let volume = Volume::open(&source).unwrap();
	let expected: Vec<_> = core::iter::once(ROOT)
		.chain(LEAVES.map(NodeId))
		.map(|id| {
			let at = CATALOG_START + u64::from(id.0) * NODE_BYTES as u64;
			(ByteOffset(at), NODE_BYTES)
		})
		.collect();
	let limits = CatalogLimits {
		max_nodes_per_query: u32::try_from(expected.len()).unwrap(),
		..CatalogLimits::default()
	};
	let catalog = volume.open_catalog(limits).unwrap();
	source.0.borrow_mut().clear();

	let mut query = catalog.query().unwrap();
	catalog.tree.audit_index_graph(&mut query).unwrap();

	assert_eq!(*source.0.borrow(), expected);
	assert_eq!(query.remaining, 0);
}

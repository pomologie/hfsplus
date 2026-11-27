use core::convert::Infallible;

use crate::alloc::{collections::BTreeSet, vec};

use super::{CatalogError, Corruption, NodeId, mark};

#[path = "tests/native_graph.rs"]
mod native_graph;

#[path = "tests/typed_graph.rs"]
mod typed_graph;

#[test]
fn visit_map_matches_a_set_through_capacity_and_repeated_scheduling() {
	for bytes in 0..=3u32 {
		let capacity = bytes * u8::BITS;
		let mut scheduled = BTreeSet::new();
		let mut visited = vec![0; bytes as usize];
		let requests = core::iter::once(capacity)
			.chain((0..capacity).rev())
			.chain(0..capacity)
			.chain([capacity, u32::MAX]);
		for index in requests {
			let id = NodeId(index);
			let expected = if index >= capacity {
				Err(Corruption::NodeAddress)
			} else if scheduled.insert(id) {
				Ok(())
			} else {
				Err(Corruption::IndexRoute)
			};
			let actual = mark::<Infallible>(id, &mut visited).map_err(|error| match error {
				CatalogError::Corrupt { node, reason } if node == id => reason,
				other => panic!("unexpected result for {id:?}: {other:?}"),
			});
			assert_eq!(actual, expected, "node {index} in a {bytes}-byte map");

			// Encode the model independently by folding its bits in node order.
			for (byte, actual) in (0..bytes).zip(&visited) {
				let first = byte * u8::BITS;
				let expected = (first..first + u8::BITS).fold(0, |value, bit| {
					value * 2 + u8::from(scheduled.contains(&NodeId(bit)))
				});
				assert_eq!(*actual, expected, "byte {byte} after node {index}");
			}
		}
		assert_eq!(scheduled.len(), capacity as usize);
	}
}

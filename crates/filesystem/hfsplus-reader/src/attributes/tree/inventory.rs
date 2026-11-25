use hfsplus_core::ReadAt;
use hfsplus_format::btree::{Kind, LEAF_HEIGHT, Node, NodeId};

use crate::alloc::vec::Vec;
use crate::attributes::AttributeReadLimits;

use super::{
	BITS_PER_MAP_BYTE, Bounds, Error, FIRST_MAP_BIT, HEADER_NODE, OwnedKey, Tree, decode_key,
	invalid_node, new_buffer, validate_keys,
};

struct Pending {
	id: NodeId,
	height: u16,
	bounds: Bounds,
}

impl<S: ReadAt> Tree<'_, S> {
	pub(in crate::attributes) fn scan_all<E: From<Error<S::Error>>>(
		&self,
		limits: AttributeReadLimits,
		mut visit: impl FnMut(NodeId, &[u8]) -> Result<(), E>,
	) -> Result<(), E> {
		if self.fork.len().0
			!= u64::from(self.header.total_nodes) * u64::from(self.header.node_bytes)
		{
			return Err(invalid_node::<S::Error>(HEADER_NODE).into());
		}
		let expected = (self.header.total_nodes - self.header.free_nodes - 1) as usize;
		let Some(root) = self.header.root else {
			return if expected == 0 {
				Ok(())
			} else {
				Err(invalid_node::<S::Error>(HEADER_NODE).into())
			};
		};
		if expected > limits.max_nodes {
			return Err(Error::NodeLimit.into());
		}
		let expected_entries = self.header.leaf_records as usize;
		if expected_entries > limits.max_entries {
			return Err(Error::EntryLimit.into());
		}
		let mut remaining = limits.max_nodes;
		let mut reached = 0;
		let mut entries = 0;
		let mut buffer = new_buffer::<S::Error>()?;
		let mut visited = Vec::new();
		visited
			.try_reserve_exact(self.allocated.len())
			.map_err(Error::Allocation)?;
		visited.resize(self.allocated.len(), 0);
		mark(root, &mut visited)?;
		let mut pending = Vec::new();
		pending.try_reserve_exact(1).map_err(Error::Allocation)?;
		pending.push(Pending {
			id: root,
			height: self.header.depth,
			bounds: Bounds::default(),
		});
		let mut previous = Vec::new();
		let level_count = usize::from(self.header.depth) + 1;
		previous
			.try_reserve_exact(level_count)
			.map_err(Error::Allocation)?;
		previous.resize(level_count, None::<(NodeId, Option<NodeId>)>);
		while let Some(Pending { id, height, bounds }) = pending.pop() {
			self.read_node(id, &mut remaining, &mut buffer)?;
			let leaf = height == u16::from(LEAF_HEIGHT);
			let kind = if leaf { Kind::Leaf } else { Kind::Index };
			let node = self.decode_node(id, &buffer, kind, height)?;
			validate_keys(id, &node, &bounds)?;
			let prior = &mut previous[usize::from(height)];
			if node.backward() != prior.map(|(previous, _)| previous)
				|| prior.is_some_and(|(_, next)| next != Some(id))
			{
				return Err(invalid_node::<S::Error>(id).into());
			}
			*prior = Some((id, node.forward()));
			reached += 1;
			if leaf {
				for raw in node.records() {
					if entries >= expected_entries {
						return Err(invalid_node::<S::Error>(id).into());
					}
					entries += 1;
					visit(id, raw)?;
				}
			} else {
				self.push_children(id, height, &node, bounds, &mut visited, &mut pending)?;
			}
		}
		if let Some((id, _)) = previous
			.into_iter()
			.flatten()
			.find(|(_, next)| next.is_some())
		{
			return Err(invalid_node::<S::Error>(id).into());
		}
		if reached != expected || entries != expected_entries {
			return Err(invalid_node::<S::Error>(HEADER_NODE).into());
		}
		Ok(())
	}

	fn push_children(
		&self,
		id: NodeId,
		height: u16,
		node: &Node<'_>,
		bounds: Bounds,
		visited: &mut [u8],
		pending: &mut Vec<Pending>,
	) -> Result<(), Error<S::Error>> {
		let mut upper = bounds.upper;
		for ordinal in (0..node.records().count()).rev() {
			let raw = node.record(ordinal).ok_or_else(|| invalid_node(id))?;
			let (key, raw_child) = decode_key(id, raw)?;
			let child = self.index_child(id, raw_child)?;
			mark(child, visited)?;
			let lower = Some(OwnedKey::new(key));
			pending.try_reserve(1).map_err(Error::Allocation)?;
			pending.push(Pending {
				id: child,
				height: height - 1,
				bounds: Bounds {
					lower: lower.clone(),
					upper,
				},
			});
			upper = lower;
		}
		Ok(())
	}
}

fn mark<E>(id: NodeId, visited: &mut [u8]) -> Result<(), Error<E>> {
	let byte = (id.0 / BITS_PER_MAP_BYTE) as usize;
	let mask = FIRST_MAP_BIT >> (id.0 % BITS_PER_MAP_BYTE);
	let byte = visited.get_mut(byte).ok_or_else(|| invalid_node(id))?;
	if *byte & mask != 0 {
		return Err(invalid_node(id));
	}
	*byte |= mask;
	Ok(())
}

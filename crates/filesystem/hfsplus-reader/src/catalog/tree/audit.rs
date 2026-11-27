use core::cmp::Ordering;

use hfsplus_core::ReadAt;

use super::{
	BITS_PER_BYTE, CatalogError, Corruption, HEADER_ID, HIGH_MAP_BIT, Kind, MIN_INDEX_CHILDREN,
	NodeId, Query, Tree, invalid, node_buffer,
};
use crate::Error;

impl<R, E> Tree<R>
where
	R: ReadAt<Error = Error<E>>,
{
	// Verify that the rooted index graph and the linked leaves describe the
	// same ordered traversal before accepting the leaf record inventory.
	pub(in crate::catalog) fn audit_index_graph(
		&self,
		query: &mut Query,
	) -> Result<(), CatalogError<E>> {
		let root = self
			.header
			.root
			.ok_or(invalid(HEADER_ID, Corruption::MissingRoot))?;
		let mut visited = node_buffer(self.map.len())?;
		let mut pending = crate::alloc::vec::Vec::new();
		pending
			.try_reserve_exact(1)
			.map_err(CatalogError::Allocation)?;
		let mut previous = crate::alloc::vec::Vec::new();
		let level_count = usize::from(self.header.depth) + 1;
		previous
			.try_reserve_exact(level_count)
			.map_err(CatalogError::Allocation)?;
		previous.resize(level_count, None);
		mark(root, &mut visited)?;
		pending.push((root, self.header.depth));
		let mut reached = self.map_nodes;
		while let Some((id, height)) = pending.pop() {
			self.check_address(id)?;
			let node = query.read(&self.fork, id)?;
			self.check_node(id, &node)?;
			if u16::from(node.height()) != height {
				return Err(invalid(id, Corruption::NodeKind));
			}
			let level = &mut previous[usize::from(height)];
			if node.backward() != level.map(|(prior, _)| prior)
				|| level.is_some_and(|(_, next)| next != Some(id))
			{
				return Err(invalid(id, Corruption::NodeLinks));
			}
			*level = Some((id, node.forward()));
			reached += 1;
			if node.kind() == Kind::Leaf {
				continue;
			}
			let count = node.records().count();
			if count < MIN_INDEX_CHILDREN {
				return Err(invalid(id, Corruption::IndexRoute));
			}
			let mut successor = None;
			for ordinal in (0..count).rev() {
				let raw = node
					.record(ordinal)
					.ok_or(invalid(id, Corruption::IndexRoute))?;
				let (key, pointer) = self.split_key(id, raw)?;
				if successor.is_some_and(|next| self.compare_keys(key, next) != Ordering::Less) {
					return Err(invalid(id, Corruption::RecordOrder));
				}
				successor = Some(key);
				let pointer: [u8; size_of::<u32>()] = pointer
					.try_into()
					.map_err(|_| invalid(id, Corruption::IndexRoute))?;
				let child = NodeId(u32::from_be_bytes(pointer));
				self.check_address(child)?;
				mark(child, &mut visited)?;
				pending.try_reserve(1).map_err(CatalogError::Allocation)?;
				pending.push((child, height - 1));
			}
		}
		if let Some((id, _)) = previous
			.into_iter()
			.flatten()
			.find(|(_, next)| next.is_some())
		{
			return Err(invalid(id, Corruption::NodeLinks));
		}
		if reached != self.header.total_nodes - self.header.free_nodes {
			return Err(invalid(HEADER_ID, Corruption::AllocationMap));
		}
		Ok(())
	}
}

fn mark<E>(id: NodeId, visited: &mut [u8]) -> Result<(), CatalogError<E>> {
	let bit = id.0 as usize;
	let mask = HIGH_MAP_BIT >> (bit % BITS_PER_BYTE);
	let byte = visited
		.get_mut(bit / BITS_PER_BYTE)
		.ok_or(invalid(id, Corruption::NodeAddress))?;
	if *byte & mask != 0 {
		return Err(invalid(id, Corruption::IndexRoute));
	}
	*byte |= mask;
	Ok(())
}

#[cfg(test)]
#[path = "audit/tests.rs"]
mod tests;

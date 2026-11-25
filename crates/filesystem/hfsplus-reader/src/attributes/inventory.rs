use hfsplus_core::ReadAt;
use hfsplus_format::{
	Error as FormatError,
	attributes::{Record, Value},
	catalog::RawName,
	volume::CatalogId,
};

use super::{AttributeReadError, Attributes};

impl<S: ReadAt> Attributes<'_, S> {
	/// Visits every allocated inline attribute by stored owner and raw name.
	/// The inventory checks the index graph, leaf chain and full allocation map;
	/// it also emits records owned by IDs absent from the catalog. The caller
	/// decides which owners are valid for its operation. Callbacks can run
	/// before a later error, so use the inventory only after success.
	/// # Errors
	/// Rejects fork-backed and unqualified attributes, corruption, I/O failures,
	/// and exhausted node or entry limits. Propagates visitor errors.
	pub fn visit_inline_attrs<E: From<AttributeReadError<S::Error>>>(
		&self,
		mut visit: impl FnMut(CatalogId, RawName<'_>, &[u8]) -> Result<(), E>,
	) -> Result<(), E> {
		let Some(tree) = &self.tree else {
			return Ok(());
		};
		let geometry = self.volume.header().geometry;
		tree.scan_all(self.limits, |node, raw| {
			let record = Record::decode(raw, geometry)
				.map_err(|error| AttributeReadError::format(node, error))?;
			match record.value {
				Value::Inline(bytes) => visit(record.key.file_id, record.key.name, bytes),
				Value::Fork(_) => Err(AttributeReadError::NonInline { node }.into()),
				_ => Err(AttributeReadError::format(node, FormatError::InvalidAttribute).into()),
			}
		})
	}
}

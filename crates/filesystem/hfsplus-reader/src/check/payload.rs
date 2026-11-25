use hfsplus_core::{ByteLen, ByteOffset, ReadAt};

use super::{AllocationOwner, CheckError, CheckLimit, StructuralReport};
use crate::Error as VolumeError;

const CHUNK_BYTES: usize = 4096;

/// Readability evidence for stored data reachable through the structural audit.
/// Use `structure` for gaps in the examined allocation or catalog profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReachableDataReport {
	/// Structural observations and coverage gaps from the same audit.
	pub structure: StructuralReport,
	/// File and attribute fork bytes read successfully, excluding slack.
	pub stored_bytes: ByteLen,
	/// Nonempty data and resource forks read, counted once per storage owner.
	pub file_forks: u64,
	/// Nonempty fork-backed attribute values read.
	pub attribute_forks: u64,
}

pub(super) struct PayloadAudit {
	limit: ByteLen,
	remaining: ByteLen,
	file_forks: u64,
	attribute_forks: u64,
}

impl PayloadAudit {
	pub(super) fn new(limit: ByteLen) -> Self {
		Self {
			limit,
			remaining: limit,
			file_forks: 0,
			attribute_forks: 0,
		}
	}

	pub(super) fn report(&self, structure: StructuralReport) -> ReachableDataReport {
		ReachableDataReport {
			structure,
			stored_bytes: ByteLen(self.limit.0 - self.remaining.0),
			file_forks: self.file_forks,
			attribute_forks: self.attribute_forks,
		}
	}

	pub(super) fn read_file<E>(
		&mut self,
		owner: AllocationOwner,
		fork: &impl ReadAt<Error = VolumeError<E>>,
	) -> Result<(), CheckError<E>> {
		self.file_forks += u64::from(self.read(owner, fork)?);
		Ok(())
	}

	pub(super) fn read_attribute<E>(
		&mut self,
		owner: AllocationOwner,
		fork: &impl ReadAt<Error = VolumeError<E>>,
	) -> Result<(), CheckError<E>> {
		self.attribute_forks += u64::from(self.read(owner, fork)?);
		Ok(())
	}

	fn read<E>(
		&mut self,
		owner: AllocationOwner,
		fork: &impl ReadAt<Error = VolumeError<E>>,
	) -> Result<bool, CheckError<E>> {
		let required = fork.len();
		if required == ByteLen(0) {
			return Ok(false);
		}
		let remaining = self
			.remaining
			.0
			.checked_sub(required.0)
			.ok_or(CheckError::Limit(CheckLimit::PayloadBytes {
				owner,
				required,
				remaining: self.remaining,
			}))?;
		let mut buffer = [0; CHUNK_BYTES];
		let mut offset = ByteOffset(0);
		while offset.0 < required.0 {
			let len = (required.0 - offset.0).min(CHUNK_BYTES as u64) as usize;
			fork.read_exact_at(offset, &mut buffer[..len])
				.map_err(|error| CheckError::PayloadRead {
					owner,
					offset,
					error,
				})?;
			offset.0 += len as u64;
		}
		self.remaining = ByteLen(remaining);
		Ok(true)
	}
}

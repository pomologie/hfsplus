//! Bounded structural and allocation checks for supported HFS+ volumes.

use hfsplus_core::{ByteLen, ReadAt};
use hfsplus_format::{Error as FormatError, catalog::FLAG_HAS_ATTRIBUTES, volume::CatalogId};

use crate::{
	AlternateHeader, Error as ReadError, Volume,
	allocation::{AllocationLimits, Claims, MetadataOwner},
	attributes::{AttributeEncoding, AttributeReadError, AttributeReadLimits},
	catalog::{CatalogLimits, EntryKind, HardLinkLimits},
	overflow::{ForkKind, OverflowLimits},
};

mod error;
mod payload;

pub use error::{CatalogProblem, CheckError, CheckLimit};
pub use payload::ReachableDataReport;

use payload::PayloadAudit;

const DEFAULT_CATALOG_ENTRIES: usize = 64 << 10;
const DEFAULT_CATALOG_NODE_READS: u32 = 1 << 20;
const DEFAULT_ALLOCATION_RANGES: usize = 1 << 18;
const DEFAULT_ATTRIBUTE_NAMES: usize = 64 << 10;

/// Work bounds for one structural check.
#[derive(Debug, Clone, Copy)]
pub struct CheckLimits {
	/// Allocation bitmap, claim count, and per-fork extent budgets. In this
	/// operation the range limit includes metadata, file and attribute claims.
	pub allocation: AllocationLimits,
	/// Catalog map, parent depth and node-read bounds. The node-read allowance
	/// applies to the complete catalog audit after its initial map is loaded.
	pub catalog: CatalogLimits,
	/// Maximum file and directory records retained by the checker.
	pub max_catalog_entries: usize,
	/// Maximum chain length per modern hard-link inode.
	pub hard_links: HardLinkLimits,
	/// Per-owner attribute limits.
	pub attributes: AttributeReadLimits,
	/// Maximum attribute names inspected across all declared owners.
	pub max_attribute_names: usize,
}

impl Default for CheckLimits {
	fn default() -> Self {
		Self {
			allocation: AllocationLimits {
				max_ranges: DEFAULT_ALLOCATION_RANGES,
				..AllocationLimits::default()
			},
			catalog: CatalogLimits {
				max_nodes_per_query: DEFAULT_CATALOG_NODE_READS,
				..CatalogLimits::default()
			},
			max_catalog_entries: DEFAULT_CATALOG_ENTRIES,
			hard_links: HardLinkLimits::default(),
			attributes: AttributeReadLimits::default(),
			max_attribute_names: DEFAULT_ATTRIBUTE_NAMES,
		}
	}
}

/// Identity of a physical allocation claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AllocationOwner {
	Metadata(MetadataOwner),
	FileFork {
		id: CatalogId,
		kind: ForkKind,
	},
	/// A stored attribute on this catalog ID. The ordinal is its zero-based
	/// position in that owner's binary name order, including inline
	/// and unsupported values.
	AttributeFork {
		id: CatalogId,
		ordinal: usize,
	},
}

impl From<MetadataOwner> for AllocationOwner {
	fn from(owner: MetadataOwner) -> Self {
		Self::Metadata(owner)
	}
}

/// Observation of the alternate volume header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AlternateStatus {
	MatchesPrimary,
	Differs,
	Blank,
	Invalid(FormatError),
}

/// Coverage of attribute metadata during the structural check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AttributeCoverage {
	Absent,
	/// Checked names and supported storage on catalog records declaring
	/// attributes. Other or missing owners may still appear in the tree.
	DeclaredOwners {
		inline: usize,
		fork_backed: usize,
		unsupported: usize,
	},
	/// Special-file allocation was checked but the tree layout is unsupported.
	UnsupportedTree,
}

/// Bounded evidence about reachable storage and catalog structure.
/// Payload authenticity, unused B-tree records outside the audited paths,
/// and unsupported profiles require separate checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct StructuralReport {
	pub total_blocks: u32,
	pub free_blocks: u32,
	pub metadata_blocks: u32,
	/// Blocks claimed by reachable file data and resource forks.
	pub reachable_fork_blocks: u32,
	/// Blocks claimed by fork-backed attributes on declared owners.
	pub attribute_fork_blocks: u32,
	/// Allocated blocks outside the verified claims. They may correspond to
	/// unsupported structures or unreachable objects.
	pub unclaimed_allocated_blocks: u32,
	/// Catalog records, including internal files when the header counts them.
	pub catalog_files: u32,
	/// Directory records below the root, including private namespaces.
	pub catalog_folders: u32,
	/// Unique regular-file hard-link inodes resolved and checked.
	pub hard_link_groups: u32,
	/// Excluded private files lacking a supported visible reference.
	pub unexamined_private_files: u32,
	/// Directory-link representations for which logical ownership is unverified.
	pub unsupported_directory_links: u32,
	/// Indicates a nonempty extents fork. Reachable chains were resolved but
	/// unreferenced overflow records in that tree were not enumerated.
	pub orphan_extent_records_unchecked: bool,
	pub alternate: AlternateStatus,
	pub attributes: AttributeCoverage,
}

impl<S: ReadAt> Volume<S> {
	/// Checks the ordered catalog leaf chain, parent graph, thread/valence and
	/// volume counters, then reconciles all supported reachable fork claims
	/// with the allocation bitmap. A shared inode claims storage once.
	///
	/// # Errors
	/// Fails on inconsistent identities or counts, malformed links or forks,
	/// overlapping claims, mismatched bits, I/O or exhausted limits. The report
	/// carries coverage gaps for unexamined private and attribute layouts.
	pub fn check_structure(
		&self,
		limits: CheckLimits,
	) -> Result<StructuralReport, CheckError<S::Error>> {
		self.check(limits, None)
	}

	/// Checks reachable structures and reads every supported stored file fork
	/// and fork-backed attribute covered by the structural audit.
	///
	/// The byte budget covers the logical length of each stored fork, excluding
	/// allocation slack and metadata reads. Hard-linked files are read once per
	/// resolved inode. Compressed forks are read in their stored representation.
	/// The report preserves the structural audit's coverage limits.
	///
	/// # Errors
	/// Returns the same structural errors as [`Self::check_structure`], as
	/// well as a payload read error or a payload byte limit error.
	pub fn check_reachable_data(
		&self,
		limits: CheckLimits,
		max_payload_bytes: ByteLen,
	) -> Result<ReachableDataReport, CheckError<S::Error>> {
		let mut payload = PayloadAudit::new(max_payload_bytes);
		let structure = self.check(limits, Some(&mut payload))?;
		Ok(payload.report(structure))
	}

	fn check(
		&self,
		limits: CheckLimits,
		mut payload: Option<&mut PayloadAudit>,
	) -> Result<StructuralReport, CheckError<S::Error>> {
		let mut allocation = self.metadata_claims(limits.allocation)?;
		let catalog = self.open_catalog(limits.catalog)?;
		let audit = catalog.audit(limits)?;
		for member in &audit.entries {
			if !member.owns_reachable_storage() {
				continue;
			}
			let EntryKind::File { data, resource } = member.entry.kind else {
				continue;
			};
			self.claim_fork(
				&mut allocation.claims,
				member.entry.metadata.id,
				ForkKind::Data,
				&data,
				limits.allocation.overflow,
				payload.as_deref_mut(),
			)?;
			self.claim_fork(
				&mut allocation.claims,
				member.entry.metadata.id,
				ForkKind::Resource,
				&resource,
				limits.allocation.overflow,
				payload.as_deref_mut(),
			)?;
		}
		let attributes = self.check_attributes(
			&mut allocation.claims,
			audit
				.entries
				.iter()
				.filter(|member| member.entry.metadata.flags & FLAG_HAS_ATTRIBUTES != 0)
				.map(|member| member.entry.metadata.id),
			limits,
			payload,
		)?;
		let checked = allocation.verify()?;
		let free_blocks = checked.free_blocks;
		let total_blocks = self.header().geometry.total_blocks();
		let metadata_blocks = checked.count(|owner| matches!(owner, AllocationOwner::Metadata(_)));
		let reachable_fork_blocks =
			checked.count(|owner| matches!(owner, AllocationOwner::FileFork { .. }));
		let attribute_fork_blocks =
			checked.count(|owner| matches!(owner, AllocationOwner::AttributeFork { .. }));
		Ok(StructuralReport {
			total_blocks,
			free_blocks,
			metadata_blocks,
			reachable_fork_blocks,
			attribute_fork_blocks,
			unclaimed_allocated_blocks: total_blocks
				- free_blocks
				- metadata_blocks
				- reachable_fork_blocks
				- attribute_fork_blocks,
			catalog_files: audit.files,
			catalog_folders: audit.folders,
			hard_link_groups: audit.link_groups,
			unexamined_private_files: audit.unexamined_private_files,
			unsupported_directory_links: audit.unsupported_directory_links,
			orphan_extent_records_unchecked: self.header().forks.extents.logical_bytes != 0,
			alternate: match self.alternate_header() {
				AlternateHeader::MatchesPrimary => AlternateStatus::MatchesPrimary,
				AlternateHeader::Differs(_) => AlternateStatus::Differs,
				AlternateHeader::Blank => AlternateStatus::Blank,
				AlternateHeader::Invalid(error) => AlternateStatus::Invalid(error),
			},
			attributes,
		})
	}

	fn claim_fork(
		&self,
		claims: &mut Claims<AllocationOwner>,
		id: CatalogId,
		kind: ForkKind,
		fork: &hfsplus_format::volume::Fork,
		overflow: OverflowLimits,
		payload: Option<&mut PayloadAudit>,
	) -> Result<(), CheckError<S::Error>> {
		let owner = AllocationOwner::FileFork { id, kind };
		let fork = self
			.open_fork(id, kind, fork, overflow)
			.map_err(|error| CheckError::Fork { owner, error })?;
		for extent in fork.extents() {
			claims.push(extent.start.0, extent.blocks, owner)?;
		}
		if let Some(payload) = payload {
			payload.read_file(owner, &fork)?;
		}
		Ok(())
	}

	fn check_attributes(
		&self,
		claims: &mut Claims<AllocationOwner>,
		owners: impl Iterator<Item = CatalogId>,
		limits: CheckLimits,
		mut payload: Option<&mut PayloadAudit>,
	) -> Result<AttributeCoverage, CheckError<S::Error>> {
		if self.header().forks.attributes.logical_bytes == 0 {
			let mut owners = owners;
			if let Some(id) = owners.next() {
				return Err(CatalogProblem::MissingAttributes(id).into());
			}
			return Ok(AttributeCoverage::Absent);
		}
		let attributes = match self.attributes(limits.attributes) {
			Ok(attributes) => attributes,
			Err(
				AttributeReadError::UnsupportedTree
				| AttributeReadError::Fork(ReadError::OverflowExtents),
			) => {
				return Ok(AttributeCoverage::UnsupportedTree);
			}
			Err(error) => return Err(CheckError::Attributes(error)),
		};
		let mut inspected = 0usize;
		let mut inline = 0usize;
		let mut fork_backed = 0usize;
		for id in owners {
			let mut ordinal = 0;
			attributes.visit_attrs::<CheckError<S::Error>>(id, |entry, fork| {
				if inspected >= limits.max_attribute_names {
					return Err(CheckError::Limit(CheckLimit::AttributeNames));
				}
				if let Some(fork) = fork {
					let owner = AllocationOwner::AttributeFork { id, ordinal };
					for extent in fork.extents() {
						claims.push(extent.start.0, extent.blocks, owner)?;
					}
					if let Some(payload) = payload.as_deref_mut() {
						payload.read_attribute(owner, &fork)?;
					}
					fork_backed += 1;
				} else if matches!(entry.encoding, AttributeEncoding::Inline(_)) {
					inline += 1;
				}
				inspected += 1;
				ordinal += 1;
				Ok(())
			})?;
			if ordinal == 0 {
				return Err(CatalogProblem::MissingAttributes(id).into());
			}
		}
		Ok(AttributeCoverage::DeclaredOwners {
			inline,
			fork_backed,
			unsupported: inspected - inline - fork_backed,
		})
	}
}

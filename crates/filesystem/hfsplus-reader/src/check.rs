//! Bounded structural and allocation checks for supported HFS+ volumes.

use hfsplus_core::ReadAt;
use hfsplus_format::{Error as FormatError, catalog::FLAG_HAS_ATTRIBUTES, volume::CatalogId};

use crate::{
	AlternateHeader, Error as ReadError, Volume,
	allocation::{AllocationLimits, Claims, MetadataOwner},
	attributes::{AttributeEncoding, AttributeReadError, AttributeReadLimits},
	catalog::{CatalogLimits, EntryKind, HardLinkLimits},
	overflow::{ForkKind, OverflowLimits},
};

mod error;

pub use error::{CatalogProblem, CheckError, CheckLimit};

const DEFAULT_CATALOG_ENTRIES: usize = 64 << 10;
const DEFAULT_CATALOG_NODE_READS: u32 = 1 << 20;
const DEFAULT_ALLOCATION_RANGES: usize = 1 << 18;
const DEFAULT_ATTRIBUTE_NAMES: usize = 64 << 10;

/// Work bounds for one structural check.
#[derive(Debug, Clone, Copy)]
pub struct CheckLimits {
	/// Allocation bitmap, claim count, and per-fork extent budgets. In this
	/// operation the range limit includes metadata and reachable file claims.
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
	FileFork { id: CatalogId, kind: ForkKind },
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
	/// Checked names on catalog records declaring attributes. Records for
	/// other or missing owners may still exist in the attributes tree.
	DeclaredOwners {
		inline: usize,
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
	pub reachable_fork_blocks: u32,
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
			)?;
			self.claim_fork(
				&mut allocation.claims,
				member.entry.metadata.id,
				ForkKind::Resource,
				&resource,
				limits.allocation.overflow,
			)?;
		}
		let attributes = self.check_attributes(
			audit
				.entries
				.iter()
				.filter(|member| member.entry.metadata.flags & FLAG_HAS_ATTRIBUTES != 0)
				.map(|member| member.entry.metadata.id),
			limits,
		)?;
		let checked = allocation.verify()?;
		let free_blocks = checked.free_blocks;
		let total_blocks = self.header().geometry.total_blocks();
		let metadata_blocks = checked.count(|owner| matches!(owner, AllocationOwner::Metadata(_)));
		let reachable_fork_blocks =
			checked.count(|owner| matches!(owner, AllocationOwner::FileFork { .. }));
		Ok(StructuralReport {
			total_blocks,
			free_blocks,
			metadata_blocks,
			reachable_fork_blocks,
			unclaimed_allocated_blocks: total_blocks
				- free_blocks
				- metadata_blocks
				- reachable_fork_blocks,
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
	) -> Result<(), CheckError<S::Error>> {
		let owner = AllocationOwner::FileFork { id, kind };
		let fork = self
			.open_fork(id, kind, fork, overflow)
			.map_err(|error| CheckError::Fork { owner, error })?;
		for extent in fork.extents() {
			claims.push(extent.start.0, extent.blocks, owner)?;
		}
		Ok(())
	}

	fn check_attributes(
		&self,
		owners: impl Iterator<Item = CatalogId>,
		limits: CheckLimits,
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
		for id in owners {
			let entries = attributes.list_attrs(id).map_err(CheckError::Attributes)?;
			if entries.is_empty() {
				return Err(CatalogProblem::MissingAttributes(id).into());
			}
			inspected = inspected
				.checked_add(entries.len())
				.filter(|&count| count <= limits.max_attribute_names)
				.ok_or(CheckError::Limit(CheckLimit::AttributeNames))?;
			inline += entries
				.iter()
				.filter(|entry| matches!(entry.encoding, AttributeEncoding::Inline(_)))
				.count();
		}
		Ok(AttributeCoverage::DeclaredOwners {
			inline,
			unsupported: inspected - inline,
		})
	}
}

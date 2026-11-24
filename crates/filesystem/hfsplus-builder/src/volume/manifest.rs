use hfsplus_core::{ReadAt, names::NormalizedName};
use hfsplus_format::volume::CatalogId;

use crate::SealedPlan;

use super::{
	EntryKind, EntryMetadata, FINDER_TYPE_AND_CREATOR_BYTES, ForkSource, MAX_SYMLINK_TARGET_BYTES,
	ManifestEntry, SYMLINK_FINDER_TYPE_AND_CREATOR, VolumeError, VolumeOptions,
	catalog_path::{NameProfile, NormalizedPath, compare_paths},
	emit,
};

pub(super) const ROOT_PARENT_ID: CatalogId = CatalogId(1);
pub(super) const ROOT_ID: CatalogId = CatalogId(2);
const POSIX_PERMISSION_MASK: u16 = 0o7777;
const INDIRECT_NODE_FINDER_CODES: &[[u8; 8]] = &[*b"hlnkhfs+", *b"fdrpMACS"];

#[derive(Debug)]
pub(super) struct PlannedEntry<'a, S> {
	pub(super) input: ManifestEntry<'a, S>,
	pub(super) path: NormalizedPath,
	pub(super) id: CatalogId,
	pub(super) parent: CatalogId,
	pub(super) valence: u32,
}

/// An admitted manifest with frozen catalog IDs and stored UTF-16 names.
#[derive(Debug)]
pub struct VolumeDraft<'a, S> {
	pub(super) options: VolumeOptions<'a>,
	pub(super) label: NormalizedName,
	profile: NameProfile,
	pub(super) entries: Vec<PlannedEntry<'a, S>>,
	pub(super) root_valence: u32,
	pub(super) folder_count: u32,
	pub(super) file_count: u32,
	pub(super) next_id: CatalogId,
}

/// An admitted manifest returned by the printable-ASCII planning entry point.
pub type AsciiVolumeDraft<'a, S> = VolumeDraft<'a, S>;

impl<S: ReadAt> VolumeDraft<'_, S> {
	/// Gets the frozen catalog ID of a path. The empty path denotes the root
	/// folder. An inadmissible path or failed normalization returns `None`.
	#[must_use]
	pub fn catalog_id(&self, path: &str) -> Option<CatalogId> {
		self.try_catalog_id(path).ok().flatten()
	}

	/// Gets the frozen catalog ID, sharing the name rules used during planning.
	/// The empty path denotes the root folder.
	///
	/// # Errors
	/// Returns an error for an inadmissible path or failed normalization.
	pub fn try_catalog_id(&self, path: &str) -> Result<Option<CatalogId>, VolumeError> {
		if path.is_empty() {
			return Ok(Some(ROOT_ID));
		}
		let normalized = NormalizedPath::new(path, self.profile)?;
		Ok(find_path(&self.entries, normalized.units()).map(|index| self.entries[index].id))
	}

	/// Supplies the data fork for a reserved generated file without changing its ID.
	///
	/// # Errors
	/// The path must identify a generated file whose data has yet to be bound.
	pub fn bind_generated_data(
		&mut self,
		path: &str,
		data: ForkSource<S>,
	) -> Result<(), VolumeError> {
		let normalized = NormalizedPath::new(path, self.profile).map_err(|error| {
			if self.profile == NameProfile::Ascii {
				VolumeError::NotAwaitingGeneratedData
			} else {
				error
			}
		})?;
		let index = find_path(&self.entries, normalized.units())
			.ok_or(VolumeError::NotAwaitingGeneratedData)?;
		let kind = &mut self.entries[index].input.kind;
		let EntryKind::GeneratedFile { resource } = kind else {
			return Err(VolumeError::NotAwaitingGeneratedData);
		};
		*kind = EntryKind::File {
			data,
			resource: resource.take(),
		};
		Ok(())
	}

	/// Allocates the forks and returns a sealed raw HFS+ image.
	///
	/// # Errors
	/// Rejects unbound generated data, invalid geometry, insufficient space,
	/// and metadata outside the admitted bounds.
	pub fn seal(self) -> Result<SealedPlan<S>, VolumeError> {
		emit::seal(self)
	}
}

/// Validates a manifest and freezes catalog IDs for this printable ASCII profile.
///
/// Directory parents must appear as manifest entries. Input order is ignored.
/// Names preserve spelling but sibling comparisons fold ASCII letter case.
/// Paths may not contain colon, empty components, or `.` and `..` components.
///
/// # Errors
/// Rejects names, kinds, metadata or hierarchy outside the profile, as well as
/// case-equivalent paths and manifests that exhaust the entry or CNID budget.
pub fn plan_ascii<'a, S: ReadAt>(
	options: VolumeOptions<'a>,
	entries: Vec<ManifestEntry<'a, S>>,
) -> Result<AsciiVolumeDraft<'a, S>, VolumeError> {
	prepare(options, entries, NameProfile::Ascii)
}

/// Validates a Unicode manifest and freezes its catalog IDs.
///
/// Paths are relative and every parent directory must be a separate entry.
/// Display units retain case and ignorable characters after canonical
/// decomposition. Lookup and collision checks use historical HFS+ collation.
/// Native access can require the stored spelling for supplementary
/// decompositions that macOS does not apply during path lookup.
///
/// # Errors
/// Rejects inadmissible names, normalization failures, equivalent paths,
/// invalid parents and metadata, and exhausted resource limits.
pub fn plan<'a, S: ReadAt>(
	options: VolumeOptions<'a>,
	entries: Vec<ManifestEntry<'a, S>>,
) -> Result<VolumeDraft<'a, S>, VolumeError> {
	prepare(options, entries, NameProfile::Unicode)
}

fn prepare<'a, S: ReadAt>(
	options: VolumeOptions<'a>,
	entries: Vec<ManifestEntry<'a, S>>,
	profile: NameProfile,
) -> Result<VolumeDraft<'a, S>, VolumeError> {
	let label = profile.volume_label(options.label)?;
	validate_metadata(options.root)?;
	if entries.len() > options.max_entries {
		return Err(VolumeError::Limit);
	}
	let count = u32::try_from(entries.len()).map_err(|_| VolumeError::Limit)?;
	let next_id = CatalogId(
		CatalogId::FIRST_USER
			.0
			.checked_add(count)
			.ok_or(VolumeError::Limit)?,
	);
	let mut canonical = Vec::new();
	canonical.try_reserve_exact(entries.len())?;
	for entry in entries {
		let path = NormalizedPath::new(entry.path, profile)?;
		validate_entry(&entry)?;
		canonical.push((path, entry));
	}
	canonical.sort_unstable_by(|(left, a), (right, b)| {
		compare_paths(left.units(), right.units()).then_with(|| a.path.cmp(b.path))
	});
	for pair in canonical.windows(2) {
		let ((left, a), (right, b)) = (&pair[0], &pair[1]);
		if compare_paths(left.units(), right.units()).is_eq() {
			return match profile {
				NameProfile::Ascii => Err(VolumeError::DuplicatePath),
				NameProfile::Unicode => Err(VolumeError::PathCollision {
					first: copy_path(a.path)?,
					second: copy_path(b.path)?,
				}),
			};
		}
	}

	let mut planned = Vec::new();
	planned.try_reserve_exact(canonical.len())?;
	let mut root_valence = 0;
	let mut folder_count = 0;
	for (ordinal, (path, input)) in (CatalogId::FIRST_USER.0..next_id.0).zip(canonical) {
		let parent = match path.parent() {
			None => {
				root_valence += 1;
				ROOT_ID
			}
			Some(parent_path) => {
				let index = find_path(&planned, parent_path).ok_or(VolumeError::InvalidParent)?;
				let parent = &mut planned[index];
				if !matches!(parent.input.kind, EntryKind::Folder) {
					return Err(VolumeError::InvalidParent);
				}
				parent.valence += 1;
				parent.id
			}
		};
		folder_count += u32::from(matches!(input.kind, EntryKind::Folder));
		planned.push(PlannedEntry {
			input,
			path,
			parent,
			id: CatalogId(ordinal),
			valence: 0,
		});
	}
	Ok(VolumeDraft {
		options,
		label,
		profile,
		entries: planned,
		root_valence,
		folder_count,
		file_count: count - folder_count,
		next_id,
	})
}

fn copy_path(path: &str) -> Result<String, VolumeError> {
	let mut copied = String::new();
	copied.try_reserve_exact(path.len())?;
	copied.push_str(path);
	Ok(copied)
}

fn validate_metadata(metadata: EntryMetadata) -> Result<(), VolumeError> {
	if metadata.mode & !POSIX_PERMISSION_MASK != 0 {
		return Err(VolumeError::InvalidMode);
	}
	if INDIRECT_NODE_FINDER_CODES
		.iter()
		.any(|signature| metadata.finder_info.starts_with(signature))
	{
		return Err(VolumeError::UnsupportedFinderInfo);
	}
	Ok(())
}

fn validate_entry<S>(entry: &ManifestEntry<'_, S>) -> Result<(), VolumeError> {
	validate_metadata(entry.metadata)?;
	let type_and_creator = &entry.metadata.finder_info[..FINDER_TYPE_AND_CREATOR_BYTES];
	let has_link_marker = type_and_creator == SYMLINK_FINDER_TYPE_AND_CREATOR;
	match &entry.kind {
		EntryKind::Symlink { target } => {
			if target.is_empty() || target.len() > MAX_SYMLINK_TARGET_BYTES || target.contains(&0) {
				return Err(VolumeError::InvalidSymlinkTarget);
			}
			if !has_link_marker && type_and_creator != [0; FINDER_TYPE_AND_CREATOR_BYTES] {
				return Err(VolumeError::UnsupportedFinderInfo);
			}
		}
		EntryKind::File { .. } | EntryKind::GeneratedFile { .. } if has_link_marker => {
			return Err(VolumeError::UnsupportedFinderInfo);
		}
		_ => {}
	}
	Ok(())
}

fn find_path<S>(entries: &[PlannedEntry<'_, S>], path: &[u16]) -> Option<usize> {
	entries
		.binary_search_by(|entry| compare_paths(entry.path.units(), path))
		.ok()
}

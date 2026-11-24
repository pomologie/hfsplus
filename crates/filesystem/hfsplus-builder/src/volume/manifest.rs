use std::cmp::Ordering;

use hfsplus_core::ReadAt;
use hfsplus_format::{catalog::MAX_NAME_UNITS, volume::CatalogId};

use crate::SealedPlan;

use super::{
	EntryKind, EntryMetadata, FINDER_TYPE_AND_CREATOR_BYTES, ForkSource, MAX_SYMLINK_TARGET_BYTES,
	ManifestEntry, SYMLINK_FINDER_TYPE_AND_CREATOR, VolumeError, VolumeOptions, emit,
};

pub(super) const ROOT_PARENT_ID: CatalogId = CatalogId(1);
pub(super) const ROOT_ID: CatalogId = CatalogId(2);
const PATH_SEPARATOR: char = '/';
const FORBIDDEN_NAME_BYTE: u8 = b':';
const PRINTABLE_ASCII: std::ops::RangeInclusive<u8> = b' '..=b'~';
const POSIX_PERMISSION_MASK: u16 = 0o7777;
const INDIRECT_NODE_FINDER_CODES: &[[u8; 8]] = &[*b"hlnkhfs+", *b"fdrpMACS"];

#[derive(Debug)]
pub(super) struct PlannedEntry<'a, S> {
	pub(super) input: ManifestEntry<'a, S>,
	pub(super) id: CatalogId,
	pub(super) parent: CatalogId,
	pub(super) valence: u32,
}

/// An admitted ASCII manifest with frozen catalog IDs.
#[derive(Debug)]
pub struct AsciiVolumeDraft<'a, S> {
	pub(super) options: VolumeOptions<'a>,
	pub(super) entries: Vec<PlannedEntry<'a, S>>,
	pub(super) root_valence: u32,
	pub(super) folder_count: u32,
	pub(super) file_count: u32,
	pub(super) next_id: CatalogId,
}

impl<S: ReadAt> AsciiVolumeDraft<'_, S> {
	/// Gets the frozen catalog ID of a path, using the admitted ASCII comparison.
	/// The empty path denotes the root folder.
	#[must_use]
	pub fn catalog_id(&self, path: &str) -> Option<CatalogId> {
		if path.is_empty() {
			return Some(ROOT_ID);
		}
		let index = find_path(&self.entries, path).ok()?;
		Some(self.entries[index].id)
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
		let index =
			find_path(&self.entries, path).map_err(|_| VolumeError::NotAwaitingGeneratedData)?;
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
	mut entries: Vec<ManifestEntry<'a, S>>,
) -> Result<AsciiVolumeDraft<'a, S>, VolumeError> {
	validate_name(options.label)?;
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
	for entry in &entries {
		validate_path(entry.path)?;
		validate_entry(entry)?;
	}
	entries.sort_unstable_by(|left, right| compare_paths(left.path, right.path));
	if entries
		.windows(2)
		.any(|pair| compare_paths(pair[0].path, pair[1].path).is_eq())
	{
		return Err(VolumeError::DuplicatePath);
	}

	let mut planned = Vec::new();
	planned.try_reserve_exact(entries.len())?;
	let mut root_valence = 0;
	let mut folder_count = 0;
	for (ordinal, input) in (CatalogId::FIRST_USER.0..next_id.0).zip(entries) {
		let parent = match input.path.rsplit_once(PATH_SEPARATOR) {
			None => {
				root_valence += 1;
				ROOT_ID
			}
			Some((path, _)) => {
				let index = find_path(&planned, path).map_err(|_| VolumeError::InvalidParent)?;
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
			parent,
			id: CatalogId(ordinal),
			valence: 0,
		});
	}
	Ok(AsciiVolumeDraft {
		options,
		entries: planned,
		root_valence,
		folder_count,
		file_count: count - folder_count,
		next_id,
	})
}

fn validate_name(name: &str) -> Result<(), VolumeError> {
	if name.is_empty()
		|| name.len() > MAX_NAME_UNITS
		|| !name.bytes().all(|byte| {
			PRINTABLE_ASCII.contains(&byte)
				&& byte != FORBIDDEN_NAME_BYTE
				&& byte != PATH_SEPARATOR as u8
		}) {
		return Err(VolumeError::UnsupportedName);
	}
	Ok(())
}

fn validate_path(path: &str) -> Result<(), VolumeError> {
	for name in path.split(PATH_SEPARATOR) {
		if name.is_empty() || matches!(name, "." | "..") {
			return Err(VolumeError::InvalidPath);
		}
		validate_name(name)?;
	}
	Ok(())
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

pub(super) fn basename(path: &str) -> &str {
	path.rsplit_once(PATH_SEPARATOR)
		.map_or(path, |(_, name)| name)
}

pub(super) fn compare_names(left: &str, right: &str) -> Ordering {
	left.bytes()
		.map(|byte| byte.to_ascii_lowercase())
		.cmp(right.bytes().map(|byte| byte.to_ascii_lowercase()))
}

fn find_path<S>(entries: &[PlannedEntry<'_, S>], path: &str) -> Result<usize, usize> {
	entries.binary_search_by(|entry| compare_paths(entry.input.path, path))
}

fn compare_paths(left: &str, right: &str) -> Ordering {
	let mut right = right.split(PATH_SEPARATOR);
	for left in left.split(PATH_SEPARATOR) {
		let Some(right) = right.next() else {
			return Ordering::Greater;
		};
		let order = compare_names(left, right);
		if !order.is_eq() {
			return order;
		}
	}
	if right.next().is_some() {
		Ordering::Less
	} else {
		Ordering::Equal
	}
}

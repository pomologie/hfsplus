use hfsplus_core::{
	ReadAt,
	names::{NameCollation, NormalizedName},
};
use hfsplus_format::{
	symlink::{FINDER_TYPE_AND_CREATOR, validate_target},
	volume::CatalogId,
};

use crate::SealedPlan;

use super::{
	EntryKind, EntryMetadata, ForkSource, HardLinkGroup, ManifestEntry, VolumeError, VolumeOptions,
	attributes::Attributes,
	catalog_path::{NameProfile, NormalizedPath, compare_paths},
	emit,
	hard_links::HardLinks,
};

pub(super) const ROOT_PARENT_ID: CatalogId = CatalogId(1);
pub(super) const ROOT_ID: CatalogId = CatalogId(2);
const POSIX_PERMISSION_MASK: u16 = 0o7777;
const INDIRECT_NODE_FINDER_CODES: &[[u8; 8]] = &[*b"hlnkhfs+", *b"fdrpMACS"];

#[derive(Debug)]
pub(super) enum Input<'a, S> {
	Ordinary(ManifestEntry<'a, S>),
	Alias { path: &'a str, group: usize },
}

impl<S> Input<'_, S> {
	fn path(&self) -> &str {
		match self {
			Self::Ordinary(entry) => entry.path,
			Self::Alias { path, .. } => path,
		}
	}

	fn is_folder(&self) -> bool {
		matches!(self, Self::Ordinary(entry) if matches!(entry.kind, EntryKind::Folder))
	}
}

#[derive(Debug)]
pub(super) struct PlannedEntry<'a, S> {
	pub(super) input: Input<'a, S>,
	pub(super) path: NormalizedPath,
	pub(super) id: CatalogId,
	pub(super) parent: CatalogId,
	pub(super) valence: u32,
	pub(super) subfolders: u32,
}

/// An admitted manifest with frozen catalog IDs and stored UTF-16 names.
#[derive(Debug)]
pub struct VolumeDraft<'a, S> {
	pub(super) options: VolumeOptions<'a>,
	pub(super) label: NormalizedName,
	profile: NameProfile,
	pub(super) entries: Vec<PlannedEntry<'a, S>>,
	pub(super) root_valence: u32,
	pub(super) root_subfolders: u32,
	pub(super) folder_count: u32,
	pub(super) file_count: u32,
	pub(super) next_id: CatalogId,
	pub(super) attributes: Attributes,
	pub(super) hard_links: Option<HardLinks<S>>,
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
		self.find_id(path, |entry| entry.id)
	}

	/// Gets the frozen inode ID of a path, shared by all aliases of a linked
	/// file. For a path with no hard links this is its catalog ID.
	#[must_use]
	pub fn inode_id(&self, path: &str) -> Option<CatalogId> {
		self.try_inode_id(path).ok().flatten()
	}

	/// Gets the frozen inode ID with the same name rules as catalog lookup.
	/// The empty path denotes the root folder.
	/// # Errors
	/// Returns an error for an inadmissible path or failed normalization.
	pub fn try_inode_id(&self, path: &str) -> Result<Option<CatalogId>, VolumeError> {
		self.find_id(path, |entry| match entry.input {
			Input::Ordinary(_) => entry.id,
			Input::Alias { group, .. } => {
				self.hard_links
					.as_ref()
					.expect("every admitted alias has a hard-link group")
					.files[group]
					.id
			}
		})
	}

	fn find_id(
		&self,
		path: &str,
		id: impl FnOnce(&PlannedEntry<'_, S>) -> CatalogId,
	) -> Result<Option<CatalogId>, VolumeError> {
		if path.is_empty() {
			return Ok(Some(ROOT_ID));
		}
		let collation = self.options.format.name_collation();
		let normalized = NormalizedPath::new(path, self.profile, collation)?;
		Ok(find_path(&self.entries, normalized.units(), collation)
			.map(|index| id(&self.entries[index])))
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
		let collation = self.options.format.name_collation();
		let normalized = NormalizedPath::new(path, self.profile, collation).map_err(|error| {
			if self.profile == NameProfile::Ascii {
				VolumeError::NotAwaitingGeneratedData
			} else {
				error
			}
		})?;
		let index = find_path(&self.entries, normalized.units(), collation)
			.ok_or(VolumeError::NotAwaitingGeneratedData)?;
		let Input::Ordinary(entry) = &mut self.entries[index].input else {
			return Err(VolumeError::NotAwaitingGeneratedData);
		};
		let kind = &mut entry.kind;
		let EntryKind::GeneratedFile { resource } = kind else {
			return Err(VolumeError::NotAwaitingGeneratedData);
		};
		*kind = EntryKind::File {
			data,
			resource: resource.take(),
		};
		Ok(())
	}

	/// Adds an inline attribute to a previously admitted catalog owner. The empty
	/// path denotes the root folder. Paths in one hard-link group share all
	/// attributes. Attribute names preserve their supplied UTF-16 spelling.
	///
	/// # Errors
	/// Rejects unknown owners, invalid names, special attributes, values requiring
	/// fork storage, and exhausted metadata budgets. Sealing rejects duplicate
	/// names on the same owner.
	pub fn add_inline_attribute(
		&mut self,
		path: &str,
		name: &str,
		value: &[u8],
	) -> Result<(), VolumeError> {
		let owner = self
			.try_inode_id(path)?
			.ok_or(VolumeError::MissingAttributeOwner)?;
		let limit = usize::try_from(self.options.limits.max_metadata_bytes.0).unwrap_or(usize::MAX);
		self.attributes.add(owner, name, value, limit)
	}

	/// Allocates the forks and returns a sealed raw volume in the selected format.
	///
	/// # Errors
	/// Rejects unbound generated data, invalid geometry, insufficient space,
	/// duplicate attribute names, and metadata outside the admitted bounds.
	pub fn seal(self) -> Result<SealedPlan<S>, VolumeError> {
		emit::seal(self)
	}
}

/// Validates a manifest and freezes catalog IDs for this printable ASCII profile.
///
/// Directory parents must appear as manifest entries. Input order is ignored.
/// Names preserve spelling. Sibling comparisons follow the volume format.
/// Paths may not contain colon, empty components, or `.` and `..` components.
///
/// # Errors
/// Rejects names, kinds, metadata or hierarchy outside the profile, as well as
/// equivalent paths and manifests that exhaust the entry or CNID budget.
pub fn plan_ascii<'a, S: ReadAt>(
	options: VolumeOptions<'a>,
	entries: Vec<ManifestEntry<'a, S>>,
) -> Result<AsciiVolumeDraft<'a, S>, VolumeError> {
	prepare(options, entries, Vec::new(), NameProfile::Ascii)
}

/// Validates a Unicode manifest and freezes its catalog IDs.
///
/// Paths are relative and every parent directory must be a separate entry.
/// Display units retain case and ignorable characters after canonical
/// decomposition. Lookup and collision checks use the selected volume format's collation.
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
	prepare(options, entries, Vec::new(), NameProfile::Unicode)
}

/// Plans regular-file hard links alongside an ordinary Unicode manifest.
/// Supply each visible path in exactly one group or ordinary manifest entry.
/// Catalog IDs remain distinct by path and each group shares an inode ID,
/// metadata and fork allocation. Directory links are outside this profile.
/// # Errors
/// Applies the same rules as [`plan`] and rejects groups with fewer than two
/// paths or too many link references for the chain-free format.
pub fn plan_with_hard_links<'a, S: ReadAt>(
	options: VolumeOptions<'a>,
	entries: Vec<ManifestEntry<'a, S>>,
	groups: Vec<HardLinkGroup<'a, S>>,
) -> Result<VolumeDraft<'a, S>, VolumeError> {
	prepare(options, entries, groups, NameProfile::Unicode)
}

fn prepare<'a, S: ReadAt>(
	options: VolumeOptions<'a>,
	entries: Vec<ManifestEntry<'a, S>>,
	groups: Vec<HardLinkGroup<'a, S>>,
	profile: NameProfile,
) -> Result<VolumeDraft<'a, S>, VolumeError> {
	let collation = options.format.name_collation();
	let label = profile.volume_label(options.label, collation)?;
	validate_metadata(options.root)?;
	let visible_entries = groups.iter().try_fold(entries.len(), |count, group| {
		if group.paths.len() < 2 {
			return Err(VolumeError::InvalidHardLinkGroup);
		}
		count
			.checked_add(group.paths.len())
			.ok_or(VolumeError::Limit)
	})?;
	if visible_entries > options.max_entries {
		return Err(VolumeError::Limit);
	}
	let count = u32::try_from(visible_entries).map_err(|_| VolumeError::Limit)?;
	let first_hidden_id = CatalogId(
		CatalogId::FIRST_USER
			.0
			.checked_add(count)
			.ok_or(VolumeError::Limit)?,
	);
	let mut canonical = Vec::new();
	canonical.try_reserve_exact(visible_entries)?;
	for entry in entries {
		let path = NormalizedPath::new(entry.path, profile, collation)?;
		validate_entry(&entry)?;
		canonical.push((path, Input::Ordinary(entry)));
	}
	for (group, input) in groups.iter().enumerate() {
		validate_metadata(input.metadata)?;
		validate_file_marker(input.metadata)?;
		for &path in &input.paths {
			let normalized = NormalizedPath::new(path, profile, collation)?;
			canonical.push((normalized, Input::Alias { path, group }));
		}
	}
	canonical.sort_unstable_by(|(left, a), (right, b)| {
		compare_paths(left.units(), right.units(), collation).then_with(|| a.path().cmp(b.path()))
	});
	for pair in canonical.windows(2) {
		let ((left, a), (right, b)) = (&pair[0], &pair[1]);
		if compare_paths(left.units(), right.units(), collation).is_eq() {
			return match profile {
				NameProfile::Ascii => Err(VolumeError::DuplicatePath),
				NameProfile::Unicode => Err(VolumeError::PathCollision {
					first: copy_path(a.path())?,
					second: copy_path(b.path())?,
				}),
			};
		}
	}

	let mut planned = Vec::new();
	planned.try_reserve_exact(canonical.len())?;
	let mut root_valence = 0;
	let mut root_subfolders = 0;
	let mut folder_count = 0;
	for (ordinal, (path, input)) in (CatalogId::FIRST_USER.0..first_hidden_id.0).zip(canonical) {
		let folder = u32::from(input.is_folder());
		let parent = match path.parent() {
			None => {
				root_valence += 1;
				root_subfolders += folder;
				ROOT_ID
			}
			Some(parent_path) => {
				let index = find_path(&planned, parent_path, collation)
					.ok_or(VolumeError::InvalidParent)?;
				let parent = &mut planned[index];
				if !parent.input.is_folder() {
					return Err(VolumeError::InvalidParent);
				}
				parent.valence += 1;
				parent.subfolders += folder;
				parent.id
			}
		};
		folder_count += folder;
		planned.push(PlannedEntry {
			input,
			path,
			parent,
			id: CatalogId(ordinal),
			valence: 0,
			subfolders: 0,
		});
	}
	let hidden_folders = u32::from(!groups.is_empty());
	let hidden_files = u32::try_from(groups.len()).map_err(|_| VolumeError::Limit)?;
	let (hard_links, next_id) = HardLinks::assign(&mut planned, groups, first_hidden_id)?;
	Ok(VolumeDraft {
		options,
		label,
		profile,
		entries: planned,
		root_valence: root_valence + hidden_folders,
		root_subfolders: root_subfolders + hidden_folders,
		folder_count: folder_count + hidden_folders,
		file_count: count - folder_count + hidden_files,
		next_id,
		attributes: Attributes::default(),
		hard_links,
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
	match &entry.kind {
		EntryKind::Symlink { target } => {
			validate_target(target).map_err(|_| VolumeError::InvalidSymlinkTarget)?;
			let type_and_creator = &entry.metadata.finder_info[..FINDER_TYPE_AND_CREATOR.len()];
			if type_and_creator != FINDER_TYPE_AND_CREATOR
				&& type_and_creator != [0; FINDER_TYPE_AND_CREATOR.len()]
			{
				return Err(VolumeError::UnsupportedFinderInfo);
			}
		}
		EntryKind::File { .. } | EntryKind::GeneratedFile { .. } => {
			validate_file_marker(entry.metadata)?
		}
		_ => {}
	}
	Ok(())
}

fn validate_file_marker(metadata: EntryMetadata) -> Result<(), VolumeError> {
	if metadata.finder_info.starts_with(&FINDER_TYPE_AND_CREATOR) {
		return Err(VolumeError::UnsupportedFinderInfo);
	}
	Ok(())
}

fn find_path<S>(
	entries: &[PlannedEntry<'_, S>],
	path: &[u16],
	collation: NameCollation,
) -> Option<usize> {
	entries
		.binary_search_by(|entry| compare_paths(entry.path.units(), path, collation))
		.ok()
}

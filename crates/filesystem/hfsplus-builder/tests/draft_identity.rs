//! Production-linked finite state coverage for H-V05 and H-BUILD-01.

use std::{env, fs::File};

use hfsplus_builder::{
	SealedPlan,
	volume::{
		CapacityError, CapacityPolicy, EntryKind, EntryMetadata, ForkSource, HardLinkGroup,
		ManifestEntry, VolumeDraft, VolumeError, VolumeOptions, plan_with_hard_links,
	},
};
use hfsplus_core::{ByteLen, ByteOffset, ReadAt};
use hfsplus_format::volume::{CatalogId, Geometry};

#[path = "draft_identity/catalog.rs"]
mod catalog;

use catalog::assert_sealed_identity;

const LABEL: &str = "Stable identities";
const BLOCK_BYTES: u32 = 4096;
const RESERVED_BLOCKS: u32 = 64;
const ROOT: CatalogId = CatalogId(2);
const ROOT_PARENT: CatalogId = CatalogId(1);
const FOLDER_MODE: u16 = 0o755;
const FILE_MODE: u16 = 0o644;
const LINK_MODE: u16 = 0o777;
const PRIVATE_DIRECTORY_COUNT: usize = 1;
const PRIVATE_INODE_COUNT: usize = 1;
const ROOT_COUNT: usize = 1;
const STATIC_DATA: &[u8] = b"ordinary data";
const STATIC_RESOURCE: &[u8] = b"resource";
const SHARED_DATA: &[u8] = b"shared identity";
const LINK_TARGET: &[u8] = b"/Applications";
const UNBOUND_PATHS: [&str; 3] = [".DS_Store", "a/Nested/Asset", "a/nested/Sibling"];
const FOLDED_LOOKUPS: [&str; UNBOUND_PATHS.len()] =
	[".ds_store", "A/NESTED/asset", "A/NESTED/SIBLING"];
const ALIASES: [&str; 2] = ["a/Branch", "b/shared"];
const LONG_PATH: &str = concat!(
	"a/Nested/",
	"LongerNameLongerNameLongerNameLongerNameLongerName",
	"LongerNameLongerNameLongerNameLongerNameLongerName",
	"LongerNameLongerNameLongerNameLongerNameLongerName",
	"LongerNameLongerNameLongerNameLongerNameLongerName",
);
const BIND_ORDERS: [[usize; UNBOUND_PATHS.len()]; 6] = [
	[0, 1, 2],
	[0, 2, 1],
	[1, 0, 2],
	[1, 2, 0],
	[2, 0, 1],
	[2, 1, 0],
];
const ID_BYTES: usize = size_of::<u32>();
const ALIAS_WITNESS_BYTES: usize = ID_BYTES * 2;
const LENGTHS: [usize; UNBOUND_PATHS.len()] = [0, ALIAS_WITNESS_BYTES, BLOCK_BYTES as usize + 1];
const CAPACITY: CapacityPolicy = CapacityPolicy::Auto {
	free_space: ByteLen(0),
};
const WITNESS_OUTPUT_ENV: &str = "POMOLOGIE_FROZEN_DRAFT_OUTPUT";
const WITNESS_REVERSE_ENV: &str = "POMOLOGIE_RAW_HFS_WITNESS_REVERSE";
const WITNESS_CAPACITY: CapacityPolicy = CapacityPolicy::Exact {
	bytes: ByteLen(8 * 1024 * 1024),
	free_space: ByteLen(0),
};

#[derive(Clone, Copy)]
enum SpecKind {
	Folder,
	File,
	Generated,
	Alias,
	Symlink,
}

struct Spec {
	path: &'static str,
	kind: SpecKind,
}

impl Spec {
	const fn new(path: &'static str, kind: SpecKind) -> Self {
		Self { path, kind }
	}
}

// This reference list is already in component-wise HFS+ ASCII order.
// In particular the descendants of A precede the otherwise similar a-keep.
const SPECS: [Spec; 13] = [
	Spec::new(UNBOUND_PATHS[0], SpecKind::Generated),
	Spec::new("A", SpecKind::Folder),
	Spec::new(ALIASES[0], SpecKind::Alias),
	Spec::new("a/nested", SpecKind::Folder),
	Spec::new(UNBOUND_PATHS[1], SpecKind::Generated),
	Spec::new(LONG_PATH, SpecKind::File),
	Spec::new(UNBOUND_PATHS[2], SpecKind::Generated),
	Spec::new("a-keep", SpecKind::File),
	Spec::new("b", SpecKind::Folder),
	Spec::new(ALIASES[1], SpecKind::Alias),
	Spec::new("b/sibling", SpecKind::File),
	Spec::new("b/Target", SpecKind::Symlink),
	Spec::new("c", SpecKind::File),
];
const PRIVATE_DIRECTORY: CatalogId = CatalogId(CatalogId::FIRST_USER.0 + SPECS.len() as u32);
const SHARED_INODE: CatalogId = CatalogId(PRIVATE_DIRECTORY.0 + PRIVATE_DIRECTORY_COUNT as u32);
const OBJECT_COUNT: usize =
	ROOT_COUNT + SPECS.len() + PRIVATE_DIRECTORY_COUNT + PRIVATE_INODE_COUNT;

type Plan = SealedPlan<Payload>;
type Draft = VolumeDraft<'static, Payload>;

#[derive(Debug)]
struct Payload(Vec<u8>);

impl ReadAt for Payload {
	type Error = <[u8] as ReadAt>::Error;

	fn len(&self) -> ByteLen {
		self.0.as_slice().len().into()
	}

	fn read_exact_at(&self, offset: ByteOffset, bytes: &mut [u8]) -> Result<(), Self::Error> {
		self.0.as_slice().read_exact_at(offset, bytes)
	}
}

fn source(bytes: Vec<u8>) -> ForkSource<Payload> {
	ForkSource::whole(Payload(bytes))
}

fn reference_id(path: &str) -> CatalogId {
	let ordinal = SPECS
		.iter()
		.position(|spec| spec.path.eq_ignore_ascii_case(path))
		.unwrap();
	CatalogId(CatalogId::FIRST_USER.0 + u32::try_from(ordinal).unwrap())
}

fn generated(path: &'static str) -> ManifestEntry<'static, Payload> {
	ManifestEntry {
		path,
		metadata: EntryMetadata::new(FILE_MODE),
		kind: EntryKind::GeneratedFile {
			resource: Some(source(STATIC_RESOURCE.to_vec())),
		},
	}
}

fn inputs() -> Vec<ManifestEntry<'static, Payload>> {
	SPECS
		.iter()
		.filter_map(|spec| {
			let (mode, kind) = match spec.kind {
				SpecKind::Generated => return Some(generated(spec.path)),
				SpecKind::Alias => return None,
				SpecKind::Folder => (FOLDER_MODE, EntryKind::Folder),
				SpecKind::File => (
					FILE_MODE,
					EntryKind::File {
						data: source(STATIC_DATA.to_vec()),
						resource: None,
					},
				),
				SpecKind::Symlink => (
					LINK_MODE,
					EntryKind::Symlink {
						target: LINK_TARGET.to_vec(),
					},
				),
			};
			Some(ManifestEntry {
				path: spec.path,
				metadata: EntryMetadata::new(mode),
				kind,
			})
		})
		.collect()
}

fn link_group(reverse: bool) -> HardLinkGroup<'static, Payload> {
	let mut paths = ALIASES.to_vec();
	if reverse {
		paths.reverse();
	}
	HardLinkGroup {
		paths,
		metadata: EntryMetadata::new(FILE_MODE),
		data: source(SHARED_DATA.to_vec()),
		resource: None,
	}
}

fn draft(order: usize) -> Draft {
	let mut entries = inputs();
	let entry_count = entries.len();
	entries.rotate_left(order % entry_count);
	if order.is_multiple_of(2) {
		entries.reverse();
	}
	let geometry = Geometry::new(
		BLOCK_BYTES,
		RESERVED_BLOCKS,
		u64::from(BLOCK_BYTES) * u64::from(RESERVED_BLOCKS),
	)
	.unwrap();
	let mut options = VolumeOptions::new(LABEL, geometry);
	options.max_entries = SPECS.len();
	plan_with_hard_links(options, entries, vec![link_group(order.is_multiple_of(2))]).unwrap()
}

fn assert_frozen_identity(draft: &Draft) {
	assert_eq!(draft.try_catalog_id("").unwrap(), Some(ROOT));
	assert_eq!(draft.try_inode_id("").unwrap(), Some(ROOT));
	for spec in &SPECS {
		let id = reference_id(spec.path);
		assert_eq!(draft.try_catalog_id(spec.path).unwrap(), Some(id));
		let inode = match spec.kind {
			SpecKind::Alias => SHARED_INODE,
			_ => id,
		};
		assert_eq!(draft.try_inode_id(spec.path).unwrap(), Some(inode));
	}
	assert_eq!(draft.try_catalog_id("missing").unwrap(), None);
}

fn assert_rejected_bindings(draft: &mut Draft, bound: &[bool; UNBOUND_PATHS.len()]) {
	let wrong_paths = ["A", "a-keep", ALIASES[0], "missing"];
	for path in wrong_paths
		.into_iter()
		.chain((0..bound.len()).filter_map(|index| bound[index].then_some(FOLDED_LOOKUPS[index])))
	{
		assert!(matches!(
			draft.bind_generated_data(path, source(b"wrong binding".to_vec())),
			Err(VolumeError::NotAwaitingGeneratedData)
		));
	}
	assert!(matches!(
		draft.bind_generated_data("a//invalid", source(Vec::new())),
		Err(VolumeError::InvalidPath)
	));
	assert_frozen_identity(draft);
}

fn payload(index: usize, length: usize, asset: CatalogId, target: CatalogId) -> Vec<u8> {
	let mut bytes = vec![u8::try_from(index + 1).unwrap(); length];
	if length >= ALIAS_WITNESS_BYTES {
		bytes[..ID_BYTES].copy_from_slice(&asset.0.to_be_bytes());
		bytes[ID_BYTES..ALIAS_WITNESS_BYTES].copy_from_slice(&target.0.to_be_bytes());
	}
	bytes
}

fn first_pending(bound: &[bool; UNBOUND_PATHS.len()]) -> Option<CatalogId> {
	bound
		.iter()
		.zip(UNBOUND_PATHS)
		.filter(|(bound, _)| !**bound)
		.map(|(_, path)| reference_id(path))
		.min()
}

#[test]
fn preserves_reference_ids_across_generated_binding_states_and_seal() {
	assert_eq!(OBJECT_COUNT, 16);
	let (mut pending, mut sealed) = (0, 0);
	for (order_idx, order) in BIND_ORDERS.into_iter().enumerate() {
		for length_rotation in 0..LENGTHS.len() {
			// The same reservations and bind order visit every prefix. Sealing
			// consumes the draft, so replay each prefix through the public API.
			for prefix in 0..=order.len() {
				let mut draft = draft(order_idx + length_rotation);
				let mut bound = [false; UNBOUND_PATHS.len()];
				let mut bound_payloads = [const { Vec::new() }; UNBOUND_PATHS.len()];
				assert_frozen_identity(&draft);
				for &index in &order[..prefix] {
					assert_rejected_bindings(&mut draft, &bound);
					let bytes = payload(
						index,
						LENGTHS[(index + length_rotation) % LENGTHS.len()],
						draft
							.try_catalog_id(FOLDED_LOOKUPS[index])
							.unwrap()
							.unwrap(),
						draft.try_inode_id(ALIASES[0]).unwrap().unwrap(),
					);
					draft
						.bind_generated_data(FOLDED_LOOKUPS[index], source(bytes.clone()))
						.unwrap();
					bound[index] = true;
					bound_payloads[index] = bytes;
					assert_frozen_identity(&draft);
				}
				assert_rejected_bindings(&mut draft, &bound);
				if let Some(expected) = first_pending(&bound) {
					assert!(matches!(
						draft.capacity(CAPACITY),
						Err(CapacityError::Volume(VolumeError::MissingGeneratedData(id))) if id == expected
					));
					assert_frozen_identity(&draft);
					assert!(matches!(
						draft.seal(),
						Err(VolumeError::MissingGeneratedData(id)) if id == expected
					));
					pending += 1;
				} else {
					let capacity = draft.capacity(CAPACITY).unwrap();
					assert_frozen_identity(&draft);
					let image = draft.seal_with_capacity(CAPACITY).unwrap();
					assert_eq!(image.image_len(), capacity.image_bytes());
					assert_sealed_identity(&image, &bound_payloads);
					sealed += 1;
				}
			}
		}
	}
	println!(
		"HFS+ draft histories checked: {} total, {pending} pending, {sealed} sealed ({OBJECT_COUNT} catalog objects)",
		pending + sealed
	);
}

#[test]
fn rejects_reservations_that_cannot_get_a_public_identity() {
	type Rejected = fn(&VolumeError) -> bool;
	let cases: [(&str, Rejected); 6] = [
		("A//asset", |error| {
			matches!(error, VolumeError::InvalidPath)
		}),
		("a-keep/asset", |error| {
			matches!(error, VolumeError::InvalidParent)
		}),
		("missing/asset", |error| {
			matches!(error, VolumeError::InvalidParent)
		}),
		(".hfs+ private directory data\r/asset", |error| {
			matches!(error, VolumeError::ReservedPath)
		}),
		("\0\0\0\0HFS+ Private Data/asset", |error| {
			matches!(error, VolumeError::UnsupportedName)
		}),
		("A/bRANCH", |error| {
			matches!(error, VolumeError::PathCollision { first, second }
			if first == "A/bRANCH" && second == ALIASES[0])
		}),
	];
	for (path, matches_error) in cases {
		let mut entries = inputs();
		entries.push(generated(path));
		let options = VolumeOptions::for_capacity(LABEL, BLOCK_BYTES).unwrap();
		let failure = plan_with_hard_links(options, entries, vec![link_group(false)]).unwrap_err();
		assert!(matches_error(&failure), "{path:?}: {failure}");
	}
	let mut options = VolumeOptions::for_capacity(LABEL, BLOCK_BYTES).unwrap();
	options.max_entries = SPECS.len() - 1;
	assert!(matches!(
		plan_with_hard_links(options, inputs(), vec![link_group(false)]),
		Err(VolumeError::Limit)
	));
}

#[test]
#[ignore = "writes the raw frozen-identity fixture for independent verification"]
fn emit_frozen_draft() {
	let reverse = env::var_os(WITNESS_REVERSE_ENV).is_some();
	let mut draft = draft(usize::from(reverse));
	let order = if reverse {
		BIND_ORDERS.last().unwrap()
	} else {
		BIND_ORDERS.first().unwrap()
	};
	for &index in order {
		let path = UNBOUND_PATHS[index];
		let bytes = payload(
			index,
			ALIAS_WITNESS_BYTES + 1,
			draft.try_catalog_id(path).unwrap().unwrap(),
			draft.try_inode_id(ALIASES[0]).unwrap().unwrap(),
		);
		draft.bind_generated_data(path, source(bytes)).unwrap();
	}
	let image = File::create_new(env::var_os(WITNESS_OUTPUT_ENV).unwrap()).unwrap();
	draft
		.seal_with_capacity(WITNESS_CAPACITY)
		.unwrap()
		.write_to(image)
		.unwrap();
}

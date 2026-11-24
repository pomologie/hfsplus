use hfsplus_builder::{
	Limits, PlanError, RangeKind, SealedPlan,
	volume::{
		EntryKind, EntryMetadata, ForkSource, MAX_SYMLINK_TARGET_BYTES, ManifestEntry, VolumeError,
		VolumeOptions, plan_ascii,
	},
};
use hfsplus_core::ByteOffset;
use hfsplus_format::{
	btree::{Node, TreeHeader},
	catalog::{Record, ThreadKind, Value},
	volume::{CatalogId, Fork, Geometry, HEADER_BYTES, Header, PRIMARY_HEADER_OFFSET, Signature},
};

const VOLUME_LABEL: &str = "Portable HFS";
const BLOCK_BYTES: u32 = 4096;
const BLOCKS: u32 = 2048;
const IMAGE_BYTES: u64 = BLOCK_BYTES as u64 * BLOCKS as u64;
const PAYLOAD_PATH: &str = "Assets/Docs/Read Me";
const GENERATED_PATH: &str = ".DS_Store";
const PAYLOAD: &[u8] = b"data fork";
const RESOURCE: &[u8] = b"resource fork";
const GENERATED: &[u8] = b"generated";

fn geometry() -> Geometry {
	Geometry::new(BLOCK_BYTES, BLOCKS, IMAGE_BYTES).unwrap()
}

fn folder(path: &str) -> ManifestEntry<'_, &'static [u8]> {
	ManifestEntry {
		path,
		metadata: EntryMetadata::new(0o755),
		kind: EntryKind::Folder,
	}
}

fn empty_file(path: &str) -> ManifestEntry<'_, &'static [u8]> {
	ManifestEntry {
		path,
		metadata: EntryMetadata::new(0o644),
		kind: EntryKind::File {
			data: ForkSource::whole(&[][..]),
			resource: None,
		},
	}
}

fn entries() -> Vec<ManifestEntry<'static, &'static [u8]>> {
	let mut metadata = EntryMetadata::new(0o640);
	metadata.uid = 501;
	metadata.gid = 20;
	metadata.dates = [3_800_000_000; 5];
	metadata.finder_info[..8].copy_from_slice(b"TEXTttxt");
	vec![
		ManifestEntry {
			path: PAYLOAD_PATH,
			metadata,
			kind: EntryKind::File {
				data: ForkSource::new(
					b"prefix:data fork:trailer".as_slice(),
					ByteOffset(7),
					PAYLOAD.len().into(),
				)
				.unwrap(),
				resource: Some(ForkSource::whole(RESOURCE)),
			},
		},
		folder("Assets"),
		ManifestEntry {
			path: GENERATED_PATH,
			metadata: EntryMetadata::new(0o644),
			kind: EntryKind::GeneratedFile { resource: None },
		},
		folder("Assets/Docs"),
		empty_file("Assets/Docs/Empty"),
	]
}

fn sample(reverse: bool) -> SealedPlan<&'static [u8]> {
	let mut inputs = entries();
	if reverse {
		inputs.reverse();
	}
	let mut draft = plan_ascii(VolumeOptions::new(VOLUME_LABEL, geometry()), inputs).unwrap();
	assert_eq!(draft.catalog_id(""), Some(CatalogId(2)));
	assert_eq!(draft.catalog_id("assets/DOCS/read me"), Some(CatalogId(20)));
	assert_eq!(draft.catalog_id(GENERATED_PATH), Some(CatalogId(16)));
	draft
		.bind_generated_data(".ds_store", ForkSource::whole(GENERATED))
		.unwrap();
	assert_eq!(draft.catalog_id(GENERATED_PATH), Some(CatalogId(16)));
	assert!(matches!(
		draft.bind_generated_data(GENERATED_PATH, ForkSource::whole(GENERATED)),
		Err(VolumeError::NotAwaitingGeneratedData)
	));
	draft.seal().unwrap()
}

fn allocation<'a>(image: &'a [u8], fork: &Fork) -> &'a [u8] {
	let extent = fork.extents().next().unwrap();
	assert_eq!(fork.extents().count(), 1);
	let range = geometry().block_range(extent.start, extent.blocks).unwrap();
	&image[range.start as usize..range.end as usize]
}

fn logical_bytes<'a>(image: &'a [u8], fork: &Fork) -> &'a [u8] {
	&allocation(image, fork)[..fork.logical_bytes as usize]
}

fn assert_fork(image: &[u8], fork: &Fork, expected: &[u8]) {
	assert_eq!(logical_bytes(image, fork), expected);
	let slack = &allocation(image, fork)[expected.len()..];
	assert!(slack.iter().all(|&byte| byte == 0));
}

#[test]
fn writes_symbolic_links_as_exact_target_forks_and_file_threads() {
	const ABSOLUTE: (&str, &[u8]) = ("Applications", b"/Applications");
	const RELATIVE: (&str, &[u8]) = ("Relative", b"../missing/\xff");
	let mut input = EntryMetadata::new(0o751);
	input.uid = 501;
	input.gid = 20;
	input.dates = [3_800_000_000; 5];
	input.finder_info[8..].fill(0x42);
	let mut canonical = input;
	canonical.finder_info[..8].copy_from_slice(b"slnkrhap");
	let mut entries: Vec<ManifestEntry<'_, &[u8]>> = [ABSOLUTE, RELATIVE]
		.into_iter()
		.zip([input, canonical])
		.map(|((path, target), metadata)| ManifestEntry {
			path,
			metadata,
			kind: EntryKind::Symlink {
				target: target.to_vec(),
			},
		})
		.collect();
	entries.push(ManifestEntry {
		path: "Document",
		metadata: EntryMetadata::new(0o644),
		kind: EntryKind::File {
			data: ForkSource::whole(PAYLOAD),
			resource: None,
		},
	});
	let draft = plan_ascii(VolumeOptions::new(VOLUME_LABEL, geometry()), entries).unwrap();
	let plan = draft.seal().unwrap();
	let mut image = Vec::new();
	plan.write_to(&mut image).unwrap();
	let header = Header::decode(
		&image[PRIMARY_HEADER_OFFSET as usize..][..HEADER_BYTES],
		IMAGE_BYTES,
	)
	.unwrap();
	assert_eq!((header.files, header.folders), (3, 0));
	let (catalog, leaf) = catalog_leaf(&image, &header);
	assert_eq!(catalog.leaf_records, 8);
	let records: Vec<_> = leaf
		.records()
		.map(|bytes| Record::decode(bytes, geometry()).unwrap())
		.collect();
	for (path, target) in [ABSOLUTE, RELATIVE] {
		let record = records
			.iter()
			.find(|record| record.key.name.units().eq(path.encode_utf16()))
			.unwrap();
		let Value::File {
			metadata,
			data,
			resource,
		} = record.value
		else {
			panic!("expected a symbolic link file record");
		};
		assert_eq!(metadata.permissions.mode, 0o120751);
		assert_eq!(
			(metadata.permissions.uid, metadata.permissions.gid),
			(input.uid, input.gid)
		);
		assert_eq!(metadata.dates, input.dates);
		assert_eq!(metadata.finder_info, canonical.finder_info);
		assert_fork(&image, &data, target);
		assert_eq!(resource, Fork::empty());
		let thread = records
			.iter()
			.find(|thread| thread.key.parent_or_thread == metadata.id)
			.unwrap();
		assert!(
			matches!(thread.value, Value::Thread { kind: ThreadKind::File, parent: CatalogId(2), name } if name.units().eq(path.encode_utf16()))
		);
	}
	let document = records
		.iter()
		.find(|record| record.key.name.units().eq("Document".encode_utf16()))
		.unwrap();
	let Value::File { data, .. } = document.value else {
		panic!("expected a regular file record");
	};
	assert_fork(&image, &data, PAYLOAD);
}

#[test]
fn rejects_unrepresentable_link_targets_and_conflicting_finder_codes() {
	fn link(target: Vec<u8>) -> ManifestEntry<'static, &'static [u8]> {
		ManifestEntry {
			path: "Link",
			metadata: EntryMetadata::new(0o755),
			kind: EntryKind::Symlink { target },
		}
	}
	let options = || VolumeOptions::new(VOLUME_LABEL, geometry());
	for invalid in [
		Vec::new(),
		b"path\0trailer".to_vec(),
		vec![b'a'; MAX_SYMLINK_TARGET_BYTES + 1],
	] {
		assert!(matches!(
			plan_ascii(options(), vec![link(invalid)]),
			Err(VolumeError::InvalidSymlinkTarget)
		));
	}
	let max_target = || link(vec![b'a'; MAX_SYMLINK_TARGET_BYTES]);
	let plan = plan_ascii(options(), vec![max_target()])
		.unwrap()
		.seal()
		.unwrap();
	let exact_metadata_bytes: u64 = plan
		.ranges()
		.filter(|(_, kind)| *kind == RangeKind::Metadata)
		.map(|(range, _)| range.len().0)
		.sum();
	let mut insufficient = options();
	insufficient.limits.max_metadata_bytes.0 = exact_metadata_bytes - 1;
	assert!(
		plan_ascii(insufficient, vec![max_target()])
			.unwrap()
			.seal()
			.is_err()
	);
	assert!(matches!(
		plan_ascii(
			options(),
			vec![link(b"relative".to_vec()), empty_file("Link/child")]
		),
		Err(VolumeError::InvalidParent)
	));
	let mut conflicting_link = link(b"relative".to_vec());
	conflicting_link.metadata.finder_info[..8].copy_from_slice(b"TEXTttxt");
	assert!(matches!(
		plan_ascii(options(), vec![conflicting_link]),
		Err(VolumeError::UnsupportedFinderInfo)
	));
	let mut false_link = empty_file("File");
	false_link.metadata.finder_info[..8].copy_from_slice(b"slnkrhap");
	assert!(matches!(
		plan_ascii(options(), vec![false_link]),
		Err(VolumeError::UnsupportedFinderInfo)
	));
}

fn catalog_leaf<'a>(image: &'a [u8], header: &Header) -> (TreeHeader, Node<'a>) {
	let catalog = logical_bytes(image, &header.forks.catalog);
	let node_size = usize::from(TreeHeader::node_size_hint(catalog).unwrap());
	let tree = TreeHeader::decode(
		&Node::decode(&catalog[..node_size]).unwrap(),
		catalog.len() as u64,
	)
	.unwrap();
	let first = tree.first_leaf.unwrap().0 as usize * node_size;
	(
		tree,
		Node::decode(&catalog[first..first + node_size]).unwrap(),
	)
}

#[test]
fn writes_a_repeatable_clean_volume_with_exact_forks_and_threads() {
	let mut image = Vec::new();
	sample(false).write_to(&mut image).unwrap();
	let mut reordered = Vec::new();
	sample(true).write_to(&mut reordered).unwrap();
	assert_eq!(image, reordered);
	assert_eq!(image.len() as u64, IMAGE_BYTES);

	let primary = PRIMARY_HEADER_OFFSET as usize;
	let alternate = geometry().alternate_header_offset() as usize;
	let header = Header::decode(&image[primary..][..HEADER_BYTES], IMAGE_BYTES).unwrap();
	assert_eq!(
		image[primary..][..HEADER_BYTES],
		image[alternate..][..HEADER_BYTES]
	);
	assert!(image[..primary].iter().all(|&byte| byte == 0));
	assert!(
		image[alternate + HEADER_BYTES..]
			.iter()
			.all(|&byte| byte == 0)
	);
	assert_eq!(header.signature, Signature::HfsPlus);
	assert!(header.unmounted_cleanly());
	assert_eq!((header.files, header.folders), (3, 2));
	assert_eq!(header.next_catalog_id, CatalogId(21));

	let bitmap = logical_bytes(&image, &header.forks.allocation);
	assert_eq!(bitmap[0], u8::MAX);
	assert_eq!(bitmap[(BLOCKS / 8 - 1) as usize], 1);
	assert_eq!(
		bitmap.iter().map(|byte| byte.count_ones()).sum::<u32>(),
		BLOCKS - header.free_blocks
	);

	let extents = logical_bytes(&image, &header.forks.extents);
	let extents_header =
		TreeHeader::decode(&Node::decode(extents).unwrap(), extents.len() as u64).unwrap();
	assert_eq!((extents_header.depth, extents_header.leaf_records), (0, 0));

	let (tree, leaf) = catalog_leaf(&image, &header);
	assert_eq!(tree.leaf_records, 12);
	let records: Vec<_> = leaf
		.records()
		.map(|bytes| Record::decode(bytes, geometry()).unwrap())
		.collect();
	let root = &records[0];
	assert_eq!(root.key.parent_or_thread, CatalogId(1));
	assert_eq!(
		String::from_utf16(&root.key.name.units().collect::<Vec<_>>()).unwrap(),
		VOLUME_LABEL
	);
	assert!(
		matches!(root.value, Value::Folder { metadata, valence: 2, .. } if metadata.id == CatalogId(2))
	);

	let target = records
		.iter()
		.find(
			|record| matches!(record.value, Value::File { metadata, .. } if metadata.id == CatalogId(20)),
		)
		.unwrap();
	let Value::File {
		metadata,
		data,
		resource,
	} = target.value
	else {
		unreachable!();
	};
	assert_eq!(target.key.parent_or_thread, CatalogId(18));
	assert_eq!(
		(
			metadata.permissions.uid,
			metadata.permissions.gid,
			metadata.permissions.mode
		),
		(501, 20, 0o100640)
	);
	assert_eq!(metadata.dates, [3_800_000_000; 5]);
	assert_fork(&image, &data, PAYLOAD);
	assert_fork(&image, &resource, RESOURCE);

	let thread = records.last().unwrap();
	assert_eq!(thread.key.parent_or_thread, CatalogId(20));
	assert!(thread.key.name.is_empty());
	let Value::Thread {
		kind: ThreadKind::File,
		parent,
		name,
	} = thread.value
	else {
		panic!("expected a file thread");
	};
	assert_eq!(parent, target.key.parent_or_thread);
	assert!(name.units().eq(target.key.name.units()));
}

#[test]
fn rejects_unsupported_names_hierarchy_and_unbound_generated_data() {
	let options = || VolumeOptions::new(VOLUME_LABEL, geometry());
	assert!(matches!(
		plan_ascii(options(), vec![folder("café")]),
		Err(VolumeError::UnsupportedName)
	));
	assert!(matches!(
		plan_ascii(options(), vec![folder("a:b")]),
		Err(VolumeError::UnsupportedName)
	));
	assert!(matches!(
		plan_ascii(options(), vec![folder("a/../b")]),
		Err(VolumeError::InvalidPath)
	));
	assert!(matches!(
		plan_ascii(options(), vec![folder("A"), folder("a")]),
		Err(VolumeError::DuplicatePath)
	));
	assert!(matches!(
		plan_ascii(options(), vec![folder("a/b")]),
		Err(VolumeError::InvalidParent)
	));
	assert!(matches!(
		plan_ascii(options(), vec![empty_file("a"), folder("a/b")]),
		Err(VolumeError::InvalidParent)
	));

	let mut mode = empty_file("file");
	mode.metadata.mode |= 0o100000;
	assert!(matches!(
		plan_ascii(options(), vec![mode]),
		Err(VolumeError::InvalidMode)
	));
	assert!(matches!(
		plan_ascii(options(), entries()).unwrap().seal(),
		Err(VolumeError::MissingGeneratedData(CatalogId(16)))
	));
	let mut limited = options();
	limited.limits = Limits {
		max_ranges: 1,
		..Limits::default()
	};
	assert!(matches!(
		plan_ascii::<&[u8]>(limited, vec![]).unwrap().seal(),
		Err(VolumeError::Plan(PlanError::RangeLimit))
	));
}

#[test]
#[ignore = "requires the independent 7z HFS+ reader"]
fn seven_zip_reads_the_construction() {
	use std::{fs, path::PathBuf, process::Command, time::SystemTime};

	struct TempImage(PathBuf);
	impl Drop for TempImage {
		fn drop(&mut self) {
			let _ = fs::remove_file(&self.0);
		}
	}

	let stamp = SystemTime::now()
		.duration_since(SystemTime::UNIX_EPOCH)
		.unwrap()
		.as_nanos();
	let path = std::env::temp_dir().join(format!("pomologie-{}-{stamp}.hfs", std::process::id()));
	let mut file = fs::File::create_new(&path).unwrap();
	let image = TempImage(path);
	sample(false).write_to(&mut file).unwrap();
	drop(file);

	let extract = |path: &str| {
		let result = Command::new("7z")
			.args(["x", "-so", "-bd"])
			.arg(&image.0)
			.arg(format!("{VOLUME_LABEL}/{path}"))
			.output()
			.unwrap();
		assert!(
			result.status.success(),
			"{}",
			String::from_utf8_lossy(&result.stderr)
		);
		result.stdout
	};
	assert_eq!(extract(PAYLOAD_PATH), PAYLOAD);
	assert_eq!(extract(GENERATED_PATH), GENERATED);
}

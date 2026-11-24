use std::collections::BTreeMap;

use hfsplus_core::{ByteOffset, ReadAt};
use hfsplus_format::{
	btree::{Kind as NodeKind, Node, TreeHeader},
	catalog::{Record, ThreadKind, Value},
	hard_links::{Inode, LinkReference, PRIVATE_DIRECTORY_NAME, Stub},
	volume::{CatalogId, Fork, HEADER_BYTES, Header, PRIMARY_HEADER_OFFSET},
};

use super::{
	ALIASES, LABEL, OBJECT_COUNT, PRIVATE_DIRECTORY, Plan, ROOT, ROOT_PARENT, SHARED_DATA,
	SHARED_INODE, SPECS, STATIC_RESOURCE, SpecKind, UNBOUND_PATHS, reference_id,
};

#[derive(Debug, PartialEq, Eq)]
struct Location {
	parent: CatalogId,
	name: Vec<u16>,
	kind: ThreadKind,
}

struct Object {
	location: Location,
	forks: Option<(Fork, Fork)>,
	stub: Option<LinkReference>,
}

struct Catalog {
	objects: BTreeMap<CatalogId, Object>,
	threads: BTreeMap<CatalogId, Location>,
	shared_reference: LinkReference,
}

pub(super) fn assert_sealed_identity(image: &Plan, payloads: &[Vec<u8>; UNBOUND_PATHS.len()]) {
	let header = read_header(image);
	assert_eq!(header.next_catalog_id, CatalogId(SHARED_INODE.0 + 1));
	let catalog = read_catalog(image, &header);
	assert_eq!(catalog.objects.len(), OBJECT_COUNT);
	assert_eq!(catalog.threads.len(), OBJECT_COUNT);
	for (&id, object) in &catalog.objects {
		assert_eq!(catalog.threads.get(&id), Some(&object.location));
		if id != ROOT {
			assert_eq!(
				catalog.objects[&object.location.parent].location.kind,
				ThreadKind::Folder
			);
		}
	}
	let root = &catalog.objects[&ROOT].location;
	assert_eq!(root.parent, ROOT_PARENT);
	assert_eq!(root.kind, ThreadKind::Folder);
	assert!(root.name.iter().copied().eq(LABEL.encode_utf16()));
	let private = &catalog.objects[&PRIVATE_DIRECTORY].location;
	assert_eq!(private.parent, ROOT);
	assert_eq!(private.kind, ThreadKind::Folder);
	assert_eq!(private.name, PRIVATE_DIRECTORY_NAME);
	let shared = &catalog.objects[&SHARED_INODE];
	assert_eq!(shared.location.parent, PRIVATE_DIRECTORY);
	assert_eq!(shared.location.kind, ThreadKind::File);
	assert_eq!(
		read_fork(image, &header, shared.forks.unwrap().0),
		SHARED_DATA
	);

	for spec in &SPECS {
		let object = &catalog.objects[&reference_id(spec.path)];
		let (parent, name) = spec
			.path
			.rsplit_once('/')
			.map_or((ROOT, spec.path), |(parent, name)| {
				(reference_id(parent), name)
			});
		let kind = match spec.kind {
			SpecKind::Folder => ThreadKind::Folder,
			_ => ThreadKind::File,
		};
		assert_eq!(object.location.parent, parent);
		assert_eq!(object.location.kind, kind);
		assert!(object.location.name.iter().copied().eq(name.encode_utf16()));
		assert_eq!(
			object.stub,
			matches!(spec.kind, SpecKind::Alias).then_some(catalog.shared_reference)
		);
	}
	for (path, payload) in UNBOUND_PATHS.into_iter().zip(payloads) {
		let (data, resource) = catalog.objects[&reference_id(path)].forks.unwrap();
		assert_eq!(&read_fork(image, &header, data), payload);
		assert_eq!(read_fork(image, &header, resource), STATIC_RESOURCE);
	}
}

fn read_header(image: &Plan) -> Header {
	let mut bytes = [0; HEADER_BYTES];
	image
		.read_exact_at(ByteOffset(PRIMARY_HEADER_OFFSET), &mut bytes)
		.unwrap();
	Header::decode(&bytes, image.image_len().0).unwrap()
}

fn read_fork(image: &Plan, header: &Header, fork: Fork) -> Vec<u8> {
	assert!(fork.logical_bytes <= image.image_len().0);
	let mut bytes = vec![0; usize::try_from(fork.logical_bytes).unwrap()];
	if !bytes.is_empty() {
		let mut extents = fork.extents();
		let extent = extents.next().unwrap();
		assert!(extents.next().is_none());
		let range = header
			.geometry
			.block_range(extent.start, extent.blocks)
			.unwrap();
		image
			.read_exact_at(ByteOffset(range.start), &mut bytes)
			.unwrap();
	}
	bytes
}

fn read_catalog(image: &Plan, header: &Header) -> Catalog {
	let bytes = read_fork(image, header, header.forks.catalog);
	let node_bytes = usize::from(TreeHeader::node_size_hint(&bytes).unwrap());
	let tree = TreeHeader::decode(
		&Node::decode(&bytes[..node_bytes]).unwrap(),
		header.forks.catalog.logical_bytes,
	)
	.unwrap();
	let mut objects = BTreeMap::new();
	let mut threads = BTreeMap::new();
	let mut shared_reference = None;
	let mut next = tree.first_leaf;
	let mut previous = None;
	assert_ne!(tree.first_leaf, tree.last_leaf);
	for _ in 0..tree.total_nodes {
		let Some(id) = next else { break };
		let start = usize::try_from(id.0).unwrap() * node_bytes;
		let node = Node::decode(&bytes[start..start + node_bytes]).unwrap();
		assert_eq!(node.kind(), NodeKind::Leaf);
		assert_eq!(node.backward(), previous);
		for raw in node.records() {
			let record = Record::decode(raw, header.geometry).unwrap();
			let parent = record.key.parent_or_thread;
			let name = record.key.name.units().collect();
			let (id, kind, forks) = match record.value {
				Value::Folder { metadata, .. } => (metadata.id, ThreadKind::Folder, None),
				Value::File {
					metadata,
					data,
					resource,
				} => (metadata.id, ThreadKind::File, Some((data, resource))),
				Value::Thread {
					kind,
					parent: original,
					name,
				} => {
					assert!(record.key.name.is_empty());
					assert!(
						threads
							.insert(
								parent,
								Location {
									parent: original,
									name: name.units().collect(),
									kind
								}
							)
							.is_none()
					);
					continue;
				}
			};
			if id == SHARED_INODE {
				let inode = Inode::decode(raw, PRIVATE_DIRECTORY, header.geometry)
					.unwrap()
					.unwrap();
				assert_eq!(inode.link_count.get() as usize, ALIASES.len());
				assert!(shared_reference.replace(inode.reference).is_none());
			}
			let object = Object {
				location: Location { parent, name, kind },
				forks,
				stub: Stub::decode(&record).unwrap().map(|stub| stub.reference),
			};
			assert!(
				objects.insert(id, object).is_none(),
				"duplicate catalog ID {id:?}"
			);
		}
		previous = Some(id);
		next = node.forward();
	}
	assert!(next.is_none());
	assert_eq!(previous, tree.last_leaf);
	assert_eq!(tree.leaf_records as usize, objects.len() + threads.len());
	Catalog {
		objects,
		threads,
		shared_reference: shared_reference.unwrap(),
	}
}

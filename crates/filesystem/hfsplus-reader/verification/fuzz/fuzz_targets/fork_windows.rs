#![no_main]
#![forbid(unsafe_code)]

use std::sync::OnceLock;

use hfsplus_core::{ByteLen, ByteOffset, ReadAt};
use hfsplus_reader::{Volume, catalog::ROOT_ID, overflow::ForkKind};
use libfuzzer_sys::fuzz_target;

mod retained_capture;

use retained_capture::Capture;

const CHAINED: &[u8] = include_bytes!("../../../tests/fixtures/native-chained-hardlinks.bin");
const CHAIN_FREE: &[u8] = include_bytes!("../../../tests/fixtures/native-chain-free-hardlinks.bin");
const FIRST_DATA: &[u8] = b"pomologie linked file\n";
const FIRST_RESOURCE: &[u8] = b"pomologie resource\0fork\n";
const SECOND_DATA: &[u8] = b"pomologie second group\n";
const MAX_FORK_BYTES: usize = FIRST_RESOURCE.len();
static VOLUMES: OnceLock<[Volume<Capture>; 2]> = OnceLock::new();

fuzz_target!(|request: &[u8]| {
	let start_choice = usize::from(request.first().copied().unwrap_or_default());
	let length_choice = usize::from(request.get(1).copied().unwrap_or_default());
	let volumes = VOLUMES.get_or_init(|| {
		[CHAINED, CHAIN_FREE].map(|bytes| Volume::open(Capture::decode(bytes)).unwrap())
	});
	for volume in volumes {
		let catalog = volume.open_catalog(Default::default()).unwrap();
		for (name, data, resource) in [
			("one", FIRST_DATA, FIRST_RESOURCE),
			("four", SECOND_DATA, &[] as &[u8]),
		] {
			let entry = catalog.lookup(ROOT_ID, name).unwrap().unwrap();
			let file = catalog
				.regular_file(entry.metadata.id, Default::default())
				.unwrap()
				.unwrap();
			for (kind, expected) in [(ForkKind::Data, data), (ForkKind::Resource, resource)] {
				let fork = file.open_stored_fork(kind, Default::default()).unwrap();
				assert_eq!(fork.len(), expected.len().into());
				let offset = start_choice % (expected.len() + 1);
				let length = length_choice % (expected.len() - offset + 1);
				let mut scratch = [u8::MAX; MAX_FORK_BYTES];
				let output = &mut scratch[..length];
				fork.read_exact_at(ByteOffset(ByteLen::from(offset).0), output)
					.unwrap();
				assert_eq!(
					output,
					&expected[offset..offset + length],
					"{name} {kind:?}"
				);
			}
		}
	}
});

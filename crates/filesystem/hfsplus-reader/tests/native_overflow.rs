//! Native fragmented-fork witnesses for H-READ-01-T01 and H-READ-01-T02.

use std::{
	env,
	fmt::Debug,
	fs::File,
	io::Read,
	path::{Path, PathBuf},
};

use hfsplus_core::{ByteLen, ByteOffset, ByteRange, RangeError, ReadAt};
use hfsplus_format::{
	catalog::{Record, Value},
	volume::CatalogId,
};
use hfsplus_reader::{
	Error, SpecialFile, Volume,
	catalog::{CatalogLimits, EntryKind, ROOT_ID},
	overflow::{ForkKind, OverflowLimits, ResolvedFork},
};

#[path = "catalog/hosted.rs"]
mod hosted;
#[path = "native_overflow/packed_wire.rs"]
mod packed_wire;
#[path = "native_overflow/tree.rs"]
mod tree;

const NATIVE: &[u8] = include_bytes!("fixtures/native-overflow.bin");
const NATIVE_RECORD_INDEX: usize = 4;
const NATIVE_FILE_ID: CatalogId = CatalogId(18);
const NATIVE_FORK_BYTES: u64 = 5 * 1024 * 1024;
const NATIVE_CHUNK_BYTES: u64 = 256 * 1024;
const NATIVE_CONTINUATION_CHUNKS: [u64; 2] = [8, 16];
const SAMPLE_BYTES: usize = 16;
const FULL_WITNESS_ENV: &str = "POMOLOGIE_FULL_FORK_WITNESS";
const FILE_NAME: &str = "fragmented";
const FULL_READ_BYTES: usize = 64 * 1024 + 17;
const MIN_EXTENTS_WITH_TWO_CONTINUATIONS: usize = 17;

#[test]
#[ignore = "requires the disposable Apple native volume and mounted fork captures"]
fn native_full_forks_match_mounted_bytes() {
	let directory = PathBuf::from(env::var_os(FULL_WITNESS_ENV).unwrap());
	let raw = hosted::FixtureFile::open(&directory.join("native.cdr")).unwrap();
	let volume = Volume::open(raw).unwrap();
	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	let entry = catalog.lookup(ROOT_ID, FILE_NAME).unwrap().unwrap();
	let EntryKind::File { data, resource } = entry.kind else {
		panic!("expected the native fragmented regular file");
	};

	for (kind, descriptor, name) in [
		(ForkKind::Data, data, "data.bin"),
		(ForkKind::Resource, resource, "resource.bin"),
	] {
		assert!(descriptor.has_overflow());
		let fork = volume
			.open_fork(
				entry.metadata.id,
				kind,
				&descriptor,
				OverflowLimits::default(),
			)
			.unwrap();
		assert_native_fork(&fork, &directory.join(name));
	}
}

fn assert_native_fork<S: ReadAt>(fork: &ResolvedFork<'_, S>, expected_path: &Path)
where
	S::Error: Debug,
{
	let extents = fork.extents().count();
	assert!(extents >= MIN_EXTENTS_WITH_TWO_CONTINUATIONS);
	assert_eq!(fork.len(), ByteLen(NATIVE_FORK_BYTES));
	let mut native = File::open(expected_path).unwrap();
	assert_eq!(native.metadata().unwrap().len(), fork.len().0);
	let mut observed = [0; FULL_READ_BYTES];
	let mut expected = [0; FULL_READ_BYTES];
	for offset in (0..fork.len().0).step_by(FULL_READ_BYTES) {
		let count = usize::try_from((fork.len().0 - offset).min(FULL_READ_BYTES as u64)).unwrap();
		fork.read_exact_at(ByteOffset(offset), &mut observed[..count])
			.unwrap();
		native.read_exact(&mut expected[..count]).unwrap();
		assert_eq!(
			&observed[..count],
			&expected[..count],
			"{} at {offset}",
			expected_path.display()
		);
	}
	assert_eq!(native.read(&mut observed[..1]).unwrap(), 0);
	assert!(matches!(
		fork.read_exact_at(ByteOffset(fork.len().0), &mut observed[..1]),
		Err(Error::Bounds(RangeError::OutsideSource))
	));
	println!(
		"{}: matched {} native bytes across {extents} extents",
		expected_path.file_name().unwrap().display(),
		fork.len().0
	);
}

#[test]
fn native_data_and_resource_forks_cross_both_continuations() {
	let source = Slices::new(NATIVE);
	let record_bytes = source.slices[NATIVE_RECORD_INDEX].1;
	let volume = Volume::open(source).unwrap();
	let catalog = Record::decode(record_bytes, volume.header().geometry).unwrap();
	assert!(catalog.key.name.units().eq("fragmented".encode_utf16()));
	let Value::File {
		metadata,
		data,
		resource,
	} = catalog.value
	else {
		panic!("fixture record is a catalog file");
	};
	assert_eq!(metadata.id, NATIVE_FILE_ID);
	let embedded = volume
		.open_resolved_special_fork(SpecialFile::Extents, OverflowLimits::default())
		.unwrap();
	assert!(!embedded.is_empty());

	for (fork, kind, initial_byte) in [
		(&data, ForkKind::Data, 1),
		(&resource, ForkKind::Resource, 41),
	] {
		assert!(fork.has_overflow());
		assert!(matches!(
			volume.open_embedded_fork(fork),
			Err(Error::OverflowExtents)
		));
		let file = volume
			.open_fork(metadata.id, kind, fork, OverflowLimits::default())
			.unwrap();
		assert_eq!(file.len(), ByteLen(NATIVE_FORK_BYTES));
		for chunk in NATIVE_CONTINUATION_CHUNKS {
			let boundary = chunk * NATIVE_CHUNK_BYTES;
			let mut sample = [0; SAMPLE_BYTES * 2];
			file.read_exact_at(ByteOffset(boundary - SAMPLE_BYTES as u64), &mut sample)
				.unwrap();
			let byte = u8::try_from(chunk).unwrap() + initial_byte;
			assert_eq!(sample[..SAMPLE_BYTES], [byte - 1; SAMPLE_BYTES]);
			assert_eq!(sample[SAMPLE_BYTES..], [byte; SAMPLE_BYTES]);
		}
		assert!(matches!(
			file.read_exact_at(ByteOffset(file.len().0), &mut [0]),
			Err(Error::Bounds(RangeError::OutsideSource))
		));
	}
}

#[derive(Debug)]
struct Slices<'raw> {
	len: ByteLen,
	slices: Vec<(u64, &'raw [u8])>,
}

impl<'raw> Slices<'raw> {
	fn new(mut data: &'raw [u8]) -> Self {
		fn take<const N: usize>(data: &mut &[u8]) -> [u8; N] {
			let (first, rest) = data.split_first_chunk().unwrap();
			*data = rest;
			*first
		}
		let len = ByteLen(u64::from_be_bytes(take(&mut data)));
		let count = u16::from_be_bytes(take(&mut data));
		let slices = (0..count)
			.map(|_| {
				let offset = u64::from_be_bytes(take(&mut data));
				let width = u32::from_be_bytes(take(&mut data)) as usize;
				let (slice, rest) = data.split_at(width);
				data = rest;
				(offset, slice)
			})
			.collect();
		assert!(data.is_empty());
		Self { len, slices }
	}
}

impl ReadAt for Slices<'_> {
	type Error = RangeError;

	fn len(&self) -> ByteLen {
		self.len
	}

	fn read_exact_at(&self, offset: ByteOffset, out: &mut [u8]) -> Result<(), Self::Error> {
		ByteRange::new(offset, out.len().into(), self.len)?;
		let slice = self
			.slices
			.iter()
			.find_map(|(start, bytes)| {
				let relative = usize::try_from(offset.0.checked_sub(*start)?).ok()?;
				bytes.get(relative..relative.checked_add(out.len())?)
			})
			.ok_or(RangeError::OutsideSource)?;
		out.copy_from_slice(slice);
		Ok(())
	}
}

//! H-READ-01-T01 fragmented-fork witness.

use hfsplus_core::{ByteLen, ByteOffset, ByteRange, RangeError, ReadAt};
use hfsplus_format::{
	catalog::{Record, Value},
	volume::CatalogId,
};
use hfsplus_reader::{
	Error, SpecialFile, Volume,
	overflow::{ForkKind, OverflowLimits},
};

const NATIVE: &[u8] = include_bytes!("fixtures/native-overflow.bin");
const NATIVE_RECORD_INDEX: usize = 4;
const NATIVE_FILE_ID: CatalogId = CatalogId(18);
const NATIVE_FORK_BYTES: u64 = 5 * 1024 * 1024;
const NATIVE_CHUNK_BYTES: u64 = 256 * 1024;
const NATIVE_CONTINUATION_CHUNKS: [u64; 2] = [8, 16];
const SAMPLE_BYTES: usize = 16;

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

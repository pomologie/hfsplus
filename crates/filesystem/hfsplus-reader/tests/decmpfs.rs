//! H-READ-03 witnesses for native zlib metadata, stored and logical fork views.

use core::ops::Range;

use hfsplus_core::{ByteLen, ByteOffset, ByteRange, RangeError, ReadAt};
use hfsplus_format::volume::CatalogId;
use hfsplus_reader::{
	Volume,
	catalog::{CatalogLimits, HardLinkLimits, RegularFile},
	decmpfs::{LogicalDataError, LogicalDataLimits},
	overflow::{ForkKind, OverflowLimits},
};

const OPTICAL: &[u8] = include_bytes!("fixtures/optical-volume.bin");
const COMPRESSED: &[u8] = include_bytes!("fixtures/native-decmpfs-zlib.bin");
const INLINE_ID: CatalogId = CatalogId(18);

#[test]
fn ordinary_files_keep_their_logical_stored_data() {
	let volume = Volume::open(OPTICAL).unwrap();
	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	let entry = catalog
		.lookup(catalog.root().unwrap().metadata.id, "hello.txt")
		.unwrap()
		.unwrap();
	let file = catalog
		.regular_file(entry.metadata.id, HardLinkLimits::default())
		.unwrap()
		.unwrap();
	let logical = file
		.open_logical_data(LogicalDataLimits::default())
		.unwrap();
	let mut bytes = [0; b"pomologie fixture\n".len()];
	logical.read_exact_at(ByteOffset(0), &mut bytes).unwrap();
	assert_eq!(&bytes, b"pomologie fixture\n");
}

#[cfg(not(feature = "decmpfs-zlib"))]
#[test]
fn feature_off_refuses_compressed_data_while_stored_forks_remain_accessible() {
	let volume = Volume::open(Fixture::new(COMPRESSED)).unwrap();
	let file = regular_file(&volume, INLINE_ID);
	let physical = file
		.open_stored_fork(ForkKind::Data, OverflowLimits::default())
		.unwrap();
	assert!(physical.is_empty());
	assert!(matches!(
		file.open_logical_data(LogicalDataLimits::default()),
		Err(LogicalDataError::CompressionDisabled)
	));
}

#[cfg(feature = "decmpfs-zlib")]
mod enabled {
	use super::*;
	use hfsplus_reader::decmpfs::{CompressionLimit, FrameError};

	const NATIVE_PARTIAL: &[u8] = include_bytes!("fixtures/native-decmpfs-tail.bin");
	const SHARED: &[u8] = include_bytes!("fixtures/native-decmpfs-hardlinks.bin");
	const RESOURCE_ID: CatalogId = CatalogId(19);
	const CHUNK_BYTES: usize = 64 * 1024;
	const INLINE_SAMPLE: &[u8] = b"portable-zlib-inline-fixture\n";
	const INLINE_REPETITIONS: usize = 1024;
	const INLINE_EXPANSION: usize = INLINE_SAMPLE.len() * INLINE_REPETITIONS;
	const RESOURCE_EXPANSION: usize = CHUNK_BYTES * 3 + NATIVE_PARTIAL.len();
	const RESOURCE_RAW_BYTES: u64 = 1115;
	const ATTR_LEAF: usize = 5;
	const RESOURCE_FORK: usize = 6;
	const CONTROL_MAGIC: &[u8; 4] = b"fpmc";
	const INLINE_TYPE: u32 = 3;
	const RESOURCE_TYPE: u32 = 4;
	const UNSUPPORTED_TYPE: u32 = 8;
	const TYPE_OFFSET: usize = CONTROL_MAGIC.len();
	const EXPANDED_OFFSET: usize = TYPE_OFFSET + size_of::<u32>();
	const RESOURCE_DATA_OFFSET: u32 = 256;
	const MAP_OFFSET_FIELD: usize = size_of::<u32>();
	const FIRST_BLOCK_OFFSET_FIELD: usize = RESOURCE_DATA_OFFSET as usize + 2 * size_of::<u32>();

	#[test]
	fn reads_entire_native_inline_and_chunked_resource_data() {
		let volume = Volume::open(Fixture::new(COMPRESSED)).unwrap();
		let limits = LogicalDataLimits::default();
		let inline = regular_file(&volume, INLINE_ID);
		let resource = regular_file(&volume, RESOURCE_ID);
		check_bytes(&inline, &INLINE_SAMPLE.repeat(INLINE_REPETITIONS));
		check_bytes(&resource, &resource_bytes());

		let raw_data = inline
			.open_stored_fork(ForkKind::Data, OverflowLimits::default())
			.unwrap();
		assert!(raw_data.is_empty());
		let raw_resource = resource
			.open_stored_fork(ForkKind::Resource, OverflowLimits::default())
			.unwrap();
		assert_eq!(raw_resource.len(), ByteLen(RESOURCE_RAW_BYTES));
		let captured = Fixture::new(COMPRESSED);
		let mut stored = vec![0; RESOURCE_RAW_BYTES as usize];
		raw_resource
			.read_exact_at(ByteOffset(0), &mut stored)
			.unwrap();
		assert_eq!(
			stored,
			captured.bytes[captured.spans[RESOURCE_FORK].1.clone()]
		);
		let mut prefix = [0; size_of::<u32>()];
		raw_resource
			.read_exact_at(ByteOffset(0), &mut prefix)
			.unwrap();
		assert_eq!(u32::from_be_bytes(prefix), RESOURCE_DATA_OFFSET);

		let logical = resource.open_logical_data(limits).unwrap();
		let mut partial = [0; 20];
		logical
			.read_exact_at(ByteOffset(CHUNK_BYTES as u64 - 7), &mut partial)
			.unwrap();
		assert_eq!(&partial[..7], &[b'A'; 7]);
		assert_eq!(&partial[7..], &[b'B'; 13]);
		assert!(
			logical
				.read_exact_at(ByteOffset(logical.len().0), &mut [])
				.is_ok()
		);
		assert!(matches!(
			logical.read_exact_at(ByteOffset(logical.len().0), &mut [0]),
			Err(LogicalDataError::Bounds(RangeError::OutsideSource))
		));
	}

	#[test]
	fn resolves_compressed_hardlinks_to_the_actual_attribute_owner() {
		let mut source = Fixture::new(COMPRESSED);
		source.overlay(SHARED);
		let volume = Volume::open(source).unwrap();
		let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
		let root = catalog.root().unwrap().metadata.id;
		for (name, inode, expected) in [
			(
				"inline-hardlink",
				INLINE_ID,
				INLINE_SAMPLE.repeat(INLINE_REPETITIONS),
			),
			("resource-hardlink", RESOURCE_ID, resource_bytes()),
		] {
			let entry = catalog.lookup(root, name).unwrap().unwrap();
			let file = regular_file(&volume, entry.metadata.id);
			assert_ne!(file.id(), inode);
			assert_eq!(file.inode_id(), inode);
			assert_eq!(file.hard_link().unwrap().count.get(), 2);
			check_bytes(&file, &expected);
		}
	}

	#[test]
	fn rejects_unknown_types_and_expansion_budgets() {
		let mut source = Fixture::new(COMPRESSED);
		let bytes = source.slice_mut(ATTR_LEAF);
		let at = control_at(bytes, INLINE_TYPE);
		bytes[at + TYPE_OFFSET..][..size_of::<u32>()]
			.copy_from_slice(&UNSUPPORTED_TYPE.to_le_bytes());
		let volume = Volume::open(source).unwrap();
		let file = regular_file(&volume, INLINE_ID);
		assert!(matches!(
			file.open_logical_data(LogicalDataLimits::default()),
			Err(LogicalDataError::UnsupportedCompression(UNSUPPORTED_TYPE))
		));

		let volume = Volume::open(Fixture::new(COMPRESSED)).unwrap();
		let file = regular_file(&volume, RESOURCE_ID);
		let defaults = LogicalDataLimits::default();
		let limit = LogicalDataLimits {
			max_expanded_bytes: ByteLen(RESOURCE_EXPANSION as u64 - 1),
			..defaults
		};
		assert!(matches!(
			file.open_logical_data(limit),
			Err(LogicalDataError::Limit(CompressionLimit::ExpandedBytes))
		));
		let limit = LogicalDataLimits {
			max_resource_chunks: 3,
			..defaults
		};
		assert!(matches!(
			file.open_logical_data(limit),
			Err(LogicalDataError::Limit(CompressionLimit::ResourceChunks))
		));
	}

	#[test]
	fn rejects_inline_checksum_trailing_input_and_wrong_decoded_size() {
		for failure in [
			FrameError::Checksum,
			FrameError::TrailingBytes,
			FrameError::Length,
		] {
			let mut source = Fixture::new(COMPRESSED);
			let attrs = source.slice_mut(ATTR_LEAF);
			let offset = control_at(attrs, INLINE_TYPE);
			let inline_len_start = offset - size_of::<u32>();
			let len_bytes = &attrs[inline_len_start..offset];
			let len = u32::from_be_bytes(len_bytes.try_into().unwrap());
			match failure {
				FrameError::Checksum => attrs[offset + len as usize - 1] ^= 1,
				FrameError::TrailingBytes => {
					// The native alignment byte is made part of the advertised payload.
					assert_eq!(len % 2, 1);
					attrs[inline_len_start..offset].copy_from_slice(&(len + 1).to_be_bytes());
				}
				FrameError::Length => {
					let expanded = (INLINE_EXPANSION as u64 + 1).to_le_bytes();
					attrs[offset + EXPANDED_OFFSET..][..size_of::<u64>()]
						.copy_from_slice(&expanded);
				}
				_ => unreachable!(),
			}
			let volume = Volume::open(source).unwrap();
			let file = regular_file(&volume, INLINE_ID);
			let actual = file
				.open_logical_data(LogicalDataLimits::default())
				.unwrap_err();
			assert!(matches!(actual, LogicalDataError::Frame { block: 0, error }
				if core::mem::discriminant(&error) == core::mem::discriminant(&failure)));
		}
		let mut source = Fixture::new(COMPRESSED);
		let attrs = source.slice_mut(ATTR_LEAF);
		let offset = control_at(attrs, INLINE_TYPE);
		let truncated_expansion = 1u64.to_le_bytes();
		attrs[offset + EXPANDED_OFFSET..][..size_of::<u64>()].copy_from_slice(&truncated_expansion);
		let volume = Volume::open(source).unwrap();
		assert!(matches!(
			regular_file(&volume, INLINE_ID).open_logical_data(LogicalDataLimits::default()),
			Err(LogicalDataError::Frame {
				block: 0,
				error: FrameError::Length
			})
		));
	}

	#[test]
	fn rejects_resource_map_and_table_corruption_before_decoding() {
		let defaults = LogicalDataLimits::default();
		let mut source = Fixture::new(COMPRESSED);
		let fork = source.slice_mut(RESOURCE_FORK);
		let map = u32::from_be_bytes(
			fork[MAP_OFFSET_FIELD..][..size_of::<u32>()]
				.try_into()
				.unwrap(),
		) as usize;
		let relative_type = fork[map..]
			.windows(b"cmpf".len())
			.position(|window| window == b"cmpf")
			.unwrap();
		fork[map + relative_type] = b'!';
		let volume = Volume::open(source).unwrap();
		assert!(matches!(
			regular_file(&volume, RESOURCE_ID).open_logical_data(defaults),
			Err(LogicalDataError::ResourceLayout)
		));

		let mut source = Fixture::new(COMPRESSED);
		let fork = source.slice_mut(RESOURCE_FORK);
		fork[FIRST_BLOCK_OFFSET_FIELD..][..size_of::<u32>()].fill(0);
		let volume = Volume::open(source).unwrap();
		assert!(matches!(
			regular_file(&volume, RESOURCE_ID).open_logical_data(defaults),
			Err(LogicalDataError::ResourceLayout)
		));
	}

	#[test]
	fn a_corrupt_later_resource_frame_does_not_prevent_other_reads() {
		let mut source = Fixture::new(COMPRESSED);
		let bytes = source.slice_mut(ATTR_LEAF);
		let at = control_at(bytes, RESOURCE_TYPE);
		let expanded = (RESOURCE_EXPANSION as u64 + 1).to_le_bytes();
		bytes[at + EXPANDED_OFFSET..][..size_of::<u64>()].copy_from_slice(&expanded);
		let volume = Volume::open(source).unwrap();
		let file = regular_file(&volume, RESOURCE_ID);
		let logical = file
			.open_logical_data(LogicalDataLimits::default())
			.unwrap();
		let mut first = [0; 1];
		logical.read_exact_at(ByteOffset(0), &mut first).unwrap();
		assert_eq!(first, [b'A']);
		assert!(matches!(
			logical.read_exact_at(ByteOffset(CHUNK_BYTES as u64 * 3), &mut first),
			Err(LogicalDataError::Frame {
				block: 3,
				error: FrameError::Length
			})
		));
	}

	fn check_bytes(file: &RegularFile<'_, Fixture>, expected: &[u8]) {
		let data = file
			.open_logical_data(LogicalDataLimits::default())
			.unwrap();
		assert_eq!(data.len(), ByteLen::from(expected.len()));
		let mut buffer = vec![0; expected.len()];
		data.read_exact_at(ByteOffset(0), &mut buffer).unwrap();
		assert_eq!(buffer, expected);
	}

	fn resource_bytes() -> Vec<u8> {
		let mut output = Vec::with_capacity(RESOURCE_EXPANSION);
		for value in b"ABC" {
			output.extend(std::iter::repeat_n(*value, CHUNK_BYTES));
		}
		output.extend_from_slice(NATIVE_PARTIAL);
		output
	}

	fn control_at(bytes: &[u8], algorithm: u32) -> usize {
		let marker = [CONTROL_MAGIC.as_slice(), &algorithm.to_le_bytes()].concat();
		bytes
			.windows(marker.len())
			.position(|window| window == marker)
			.unwrap()
	}
}

fn regular_file(volume: &Volume<Fixture>, id: CatalogId) -> RegularFile<'_, Fixture> {
	let catalog = volume.open_catalog(CatalogLimits::default()).unwrap();
	catalog
		.regular_file(id, HardLinkLimits::default())
		.unwrap()
		.unwrap()
}

#[derive(Debug)]
struct Fixture {
	volume_len: ByteLen,
	bytes: Vec<u8>,
	spans: Vec<(u64, Range<usize>)>,
}

impl Fixture {
	fn new(capture: &[u8]) -> Self {
		let mut fixture = Self {
			volume_len: ByteLen(u64::from_be_bytes(capture.first_chunk().copied().unwrap())),
			bytes: Vec::new(),
			spans: Vec::new(),
		};
		fixture.append(capture);
		fixture
	}

	#[cfg(feature = "decmpfs-zlib")]
	fn overlay(&mut self, capture: &[u8]) {
		self.append(capture);
	}

	fn append(&mut self, capture: &[u8]) {
		fn unpack<const N: usize>(bytes: &mut &[u8]) -> [u8; N] {
			let (field, rest) = bytes.split_first_chunk().unwrap();
			*bytes = rest;
			*field
		}
		let mut unpacked = capture;
		let volume_len = ByteLen(u64::from_be_bytes(unpack(&mut unpacked)));
		let count = u16::from_be_bytes(unpack(&mut unpacked));
		assert_eq!(volume_len, self.volume_len);
		for _ in 0..count {
			let location = u64::from_be_bytes(unpack(&mut unpacked));
			let size = u32::from_be_bytes(unpack(&mut unpacked)) as usize;
			let (chunk, remaining) = unpacked.split_at(size);
			let start = self.bytes.len();
			self.spans.push((location, start..start + size));
			self.bytes.extend_from_slice(chunk);
			unpacked = remaining;
		}
		assert!(unpacked.is_empty());
	}

	#[cfg(feature = "decmpfs-zlib")]
	fn slice_mut(&mut self, ordinal: usize) -> &mut [u8] {
		let (_, range) = &self.spans[ordinal];
		&mut self.bytes[range.clone()]
	}
}

impl ReadAt for Fixture {
	type Error = RangeError;

	fn len(&self) -> ByteLen {
		self.volume_len
	}

	fn read_exact_at(&self, offset: ByteOffset, out: &mut [u8]) -> Result<(), Self::Error> {
		ByteRange::new(offset, out.len().into(), self.volume_len)?;
		let input = self
			.spans
			.iter()
			.rev()
			.find_map(|(start, range)| {
				let relative = usize::try_from(offset.0.checked_sub(*start)?).ok()?;
				self.bytes[range.clone()].get(relative..relative.checked_add(out.len())?)
			})
			.ok_or(RangeError::OutsideSource)?;
		out.copy_from_slice(input);
		Ok(())
	}
}

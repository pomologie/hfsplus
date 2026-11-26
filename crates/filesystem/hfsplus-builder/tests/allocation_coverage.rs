//! Large-layout and block-set checks for H-BUILD-02-T01.

use std::{
	io::{Seek, SeekFrom, Write},
	path::PathBuf,
};

use hfsplus_builder::{
	RangeKind, SealedPlan,
	volume::{
		CapacityError, CapacityPolicy, EntryKind, EntryMetadata, ForkSource, ManifestEntry,
		VolumeOptions, plan,
	},
};
use hfsplus_core::{ByteLen, ByteOffset, ByteRange, RangeError, ReadAt};
use hfsplus_format::volume::{
	ALTERNATE_HEADER_FROM_END, HEADER_BYTES, Header, PRIMARY_HEADER_OFFSET,
};

const BLOCK_BYTES: u64 = 4096;
const RESERVED_BYTES: u64 = 8 << 20;
const LARGE_FILE_BYTES: u64 = 3 << 30;
const NATIVE_OUTPUT: &str = "POMOLOGIE_HFS_LAYOUT_WITNESSES";
const SMALL_FIXTURE: &str = "small";
const BITS_PER_BYTE: usize = 8;
const FIRST_BITMAP_BIT: u8 = 0x80;

#[derive(Clone, Copy)]
struct Case {
	name: &'static str,
	files: u32,
	first_data_bytes: u64,
}

const CASES: [Case; 4] = [
	Case {
		name: "empty",
		files: 0,
		first_data_bytes: 0,
	},
	Case {
		name: SMALL_FIXTURE,
		files: 2,
		first_data_bytes: BLOCK_BYTES + 1,
	},
	Case {
		name: "large",
		files: 1,
		first_data_bytes: LARGE_FILE_BYTES,
	},
	Case {
		name: "many",
		files: 100_000,
		first_data_bytes: 0,
	},
];

#[derive(Debug)]
struct ZeroPayload(u64);

impl ReadAt for ZeroPayload {
	type Error = RangeError;

	fn len(&self) -> ByteLen {
		ByteLen(self.0)
	}

	fn read_exact_at(&self, offset: ByteOffset, destination: &mut [u8]) -> Result<(), Self::Error> {
		ByteRange::new(offset, destination.len().into(), self.len())?;
		destination.fill(0);
		Ok(())
	}
}

fn layout(case: Case) -> SealedPlan<ZeroPayload> {
	let names: Vec<_> = (0..case.files).map(|file| format!("f{file:05}")).collect();
	let entries = names
		.iter()
		.enumerate()
		.map(|(index, name)| ManifestEntry {
			path: name,
			metadata: EntryMetadata::new(0o644),
			kind: EntryKind::File {
				data: ForkSource::whole(ZeroPayload(if index == 0 {
					case.first_data_bytes
				} else {
					0
				})),
				resource: None,
			},
		})
		.collect();
	let options =
		VolumeOptions::for_capacity("Allocation Witness", u32::try_from(BLOCK_BYTES).unwrap())
			.unwrap();
	let mut draft = plan(options, entries).unwrap();
	let reserve = ByteLen(RESERVED_BYTES);
	let exact = |bytes| CapacityPolicy::Exact {
		bytes,
		free_space: reserve,
	};
	let selected = draft
		.capacity(CapacityPolicy::Auto {
			free_space: reserve,
		})
		.unwrap();
	assert_eq!(selected.free_bytes(), reserve);
	if case.name == SMALL_FIXTURE {
		let short = ByteLen(selected.image_bytes().0 - BLOCK_BYTES);
		assert!(
			matches!(draft.capacity(exact(short)), Err(CapacityError::InsufficientSpace { required, available })
			if required == selected.image_bytes() && available == short)
		);
	}
	let image = draft
		.seal_with_capacity(exact(selected.image_bytes()))
		.unwrap();
	assert_eq!(image.image_len(), selected.image_bytes());
	image
}

fn verify_block_set(image: &SealedPlan<ZeroPayload>, case: Case) {
	let image_bytes = image.image_len().0;
	let mut primary = [0; HEADER_BYTES];
	let mut alternate = [0; HEADER_BYTES];
	image
		.read_exact_at(ByteOffset(PRIMARY_HEADER_OFFSET), &mut primary)
		.unwrap();
	image
		.read_exact_at(
			ByteOffset(image_bytes - ALTERNATE_HEADER_FROM_END),
			&mut alternate,
		)
		.unwrap();
	assert_eq!(primary, alternate, "{} headers", case.name);
	let header = Header::decode(&primary, image_bytes).unwrap();
	assert_eq!(header.files, case.files, "{} file count", case.name);
	assert_eq!(u64::from(header.geometry.block_size()), BLOCK_BYTES);
	assert_eq!(
		u64::from(header.geometry.total_blocks()) * BLOCK_BYTES,
		image_bytes
	);

	let mut occupied = vec![false; usize::try_from(header.geometry.total_blocks()).unwrap()];
	let mut payload_bytes = 0;
	let mut end = ByteOffset(0);
	for (range, kind) in image.ranges() {
		assert_eq!(end, range.start(), "{} placement boundary", case.name);
		end = range.end();
		match kind {
			RangeKind::Zero => continue,
			RangeKind::Source => payload_bytes += range.len().0,
			RangeKind::Metadata => {}
		}
		let start = usize::try_from(range.start().0 / BLOCK_BYTES).unwrap();
		let end = usize::try_from(range.end().0.div_ceil(BLOCK_BYTES)).unwrap();
		for (block, used) in occupied.iter_mut().enumerate().take(end).skip(start) {
			assert!(!*used, "{} shares allocation block {block}", case.name);
			*used = true;
		}
	}
	assert_eq!(end.0, image_bytes, "{} tail coverage", case.name);
	assert_eq!(payload_bytes, case.first_data_bytes);
	assert!(occupied[0] && occupied[occupied.len() - 1]);
	let free = occupied.iter().filter(|&&used| !used).count();
	assert_eq!(free, usize::try_from(header.free_blocks).unwrap());
	assert_eq!(u64::try_from(free).unwrap() * BLOCK_BYTES, RESERVED_BYTES);

	let bitmap_fork = header.forks.allocation;
	let bitmap_extent = bitmap_fork.extents().next().unwrap();
	let mut bitmap = vec![0; occupied.len().div_ceil(BITS_PER_BYTE)];
	assert!(bitmap_fork.logical_bytes >= u64::try_from(bitmap.len()).unwrap());
	image
		.read_exact_at(
			ByteOffset(u64::from(bitmap_extent.start.0) * BLOCK_BYTES),
			&mut bitmap,
		)
		.unwrap();
	for (block, used) in occupied.into_iter().enumerate() {
		let marked =
			bitmap[block / BITS_PER_BYTE] & (FIRST_BITMAP_BIT >> (block % BITS_PER_BYTE)) != 0;
		assert_eq!(marked, used, "{} bitmap block {block}", case.name);
	}
}

#[test]
fn independent_block_set_matches_empty_small_large_and_many_file_layouts() {
	for case in CASES {
		verify_block_set(&layout(case), case);
	}
}

#[test]
#[ignore = "emits owned synthetic native fixtures when the verification runner supplies a directory"]
fn emit_allocation_witnesses() {
	const COPY_CHUNK: usize = 32 * 1024;
	let directory = PathBuf::from(std::env::var_os(NATIVE_OUTPUT).unwrap());
	let mut buffer = [0; COPY_CHUNK];
	for case in CASES {
		let image = layout(case);
		let path = directory.join(case.name).with_extension("hfs");
		let mut file = std::fs::File::create_new(path).unwrap();
		file.set_len(image.image_len().0).unwrap();
		// These fixtures define every file payload to be zero, so only metadata
		// needs backing storage in the disposable host image.
		for (range, _) in image
			.ranges()
			.filter(|(_, kind)| *kind == RangeKind::Metadata)
		{
			let mut position = range.start().0;
			file.seek(SeekFrom::Start(position)).unwrap();
			while position < range.end().0 {
				let count = usize::try_from(
					(range.end().0 - position).min(u64::try_from(COPY_CHUNK).unwrap()),
				)
				.unwrap();
				image
					.read_exact_at(ByteOffset(position), &mut buffer[..count])
					.unwrap();
				file.write_all(&buffer[..count]).unwrap();
				position += u64::try_from(count).unwrap();
			}
		}
	}
}

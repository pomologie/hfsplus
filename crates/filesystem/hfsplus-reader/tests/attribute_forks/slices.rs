//! Sparse native image fixture with payload-read tracking and injected read failures.

use std::cell::Cell;

use hfsplus_core::{ByteLen, ByteOffset, ByteRange, RangeError, ReadAt};

use super::FIRST_PAYLOAD;

const STRUCTURE: &[u8] = include_bytes!("../fixtures/native-attribute-structure.bin");
const ALTERNATE_HEADER: u64 = (1 << 24) - 1024;

pub(super) fn patch(data: &mut [u8], offset: u64, bytes: &[u8]) {
	let base = data.as_ptr() as usize;
	let slice = Slices::new(data).segment(offset, bytes.len()).unwrap();
	let difference = slice.as_ptr() as usize - base;
	data[difference..][..bytes.len()].copy_from_slice(bytes);
}

pub(super) struct Slices<'fixture> {
	len: ByteLen,
	ranges: Vec<(u64, &'fixture [u8])>,
	pub(super) payload_reads: Cell<usize>,
	pub(super) fail_at: Option<u64>,
	pub(super) fill_unrecorded: bool,
}

impl<'fixture> Slices<'fixture> {
	pub(super) fn new(mut data: &'fixture [u8]) -> Self {
		fn take<const N: usize>(data: &mut &[u8]) -> [u8; N] {
			let (bytes, rest) = data.split_first_chunk().unwrap();
			*data = rest;
			*bytes
		}
		let len = ByteLen(u64::from_be_bytes(take(&mut data)));
		let count = u16::from_be_bytes(take(&mut data));
		let ranges = (0..count)
			.map(|_| {
				let at = u64::from_be_bytes(take(&mut data));
				let len = u32::from_be_bytes(take(&mut data)) as usize;
				let (bytes, rest) = data.split_at(len);
				data = rest;
				(at, bytes)
			})
			.collect();
		assert!(data.is_empty());
		Self {
			len,
			ranges,
			payload_reads: Cell::new(0),
			fail_at: None,
			fill_unrecorded: false,
		}
	}

	pub(super) fn with_structure(mut self) -> Self {
		let supplemental = Self::new(STRUCTURE);
		assert_eq!(self.len, supplemental.len);
		self.ranges.extend(supplemental.ranges);
		self
	}

	pub(super) fn segment(&self, offset: u64, len: usize) -> Option<&'fixture [u8]> {
		self.ranges.iter().find_map(|(start, bytes)| {
			let at = usize::try_from(offset.checked_sub(*start)?).ok()?;
			bytes.get(at..at.checked_add(len)?)
		})
	}
}

impl ReadAt for Slices<'_> {
	type Error = RangeError;

	fn len(&self) -> ByteLen {
		self.len
	}

	fn read_exact_at(&self, offset: ByteOffset, out: &mut [u8]) -> Result<(), Self::Error> {
		ByteRange::new(offset, out.len().into(), self.len)?;
		if (FIRST_PAYLOAD..ALTERNATE_HEADER).contains(&offset.0) {
			self.payload_reads.set(self.payload_reads.get() + 1);
		}
		if self.fail_at == Some(offset.0) {
			return Err(RangeError::OutsideSource);
		}
		if let Some(bytes) = self.segment(offset.0, out.len()) {
			out.copy_from_slice(bytes);
		} else if self.fill_unrecorded {
			out.fill(0);
		} else {
			return Err(RangeError::OutsideSource);
		}
		Ok(())
	}
}

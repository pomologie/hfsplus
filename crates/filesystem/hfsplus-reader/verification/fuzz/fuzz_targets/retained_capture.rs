use hfsplus_core::{ByteLen, ByteOffset, ByteRange, RangeError, ReadAt};

pub struct Capture {
	len: ByteLen,
	ranges: Vec<(u64, &'static [u8])>,
}

impl Capture {
	pub fn decode(mut bytes: &'static [u8]) -> Self {
		fn take<const N: usize>(bytes: &mut &'static [u8]) -> [u8; N] {
			let (head, tail) = bytes.split_first_chunk().unwrap();
			*bytes = tail;
			*head
		}

		let len = ByteLen(u64::from_be_bytes(take(&mut bytes)));
		let count = u16::from_be_bytes(take(&mut bytes));
		let ranges = (0..count)
			.map(|_| {
				let start = u64::from_be_bytes(take(&mut bytes));
				let size = usize::try_from(u32::from_be_bytes(take(&mut bytes))).unwrap();
				let (slice, rest) = bytes.split_at(size);
				bytes = rest;
				(start, slice)
			})
			.collect();
		assert!(bytes.is_empty());
		Self { len, ranges }
	}
}

impl ReadAt for Capture {
	type Error = RangeError;

	fn len(&self) -> ByteLen {
		self.len
	}

	fn read_exact_at(&self, offset: ByteOffset, output: &mut [u8]) -> Result<(), Self::Error> {
		ByteRange::new(offset, output.len().into(), self.len)?;
		let bytes = self
			.ranges
			.iter()
			.find_map(|(start, data)| {
				let relative = usize::try_from(offset.0.checked_sub(*start)?).ok()?;
				data.get(relative..relative.checked_add(output.len())?)
			})
			.ok_or(RangeError::OutsideSource)?;
		output.copy_from_slice(bytes);
		Ok(())
	}
}

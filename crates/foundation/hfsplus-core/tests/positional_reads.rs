use hfsplus_core::{ByteLen, ByteOffset, ByteRange, RangeError, ReadAt, Window, WindowError};

#[test]
fn rejects_range_overflow_and_reads_past_eof() {
	assert_eq!(
		ByteRange::new(ByteOffset(u64::MAX), ByteLen(1), ByteLen(u64::MAX)),
		Err(RangeError::Overflow)
	);
	assert_eq!(
		b"image".as_slice().read_exact_at(ByteOffset(5), &mut [0]),
		Err(RangeError::OutsideSource)
	);
	assert!(
		b"image"
			.as_slice()
			.read_exact_at(ByteOffset(5), &mut [])
			.is_ok()
	);
}

#[test]
fn nested_windows_keep_relative_bounds() {
	let window = Window::new(b"image".as_slice(), ByteOffset(1), ByteLen(3)).unwrap();
	let window = Window::new(window, ByteOffset(1), ByteLen(2)).unwrap();
	let mut bytes = [0; 2];
	window.read_exact_at(ByteOffset(0), &mut bytes).unwrap();
	assert_eq!(&bytes, b"ag");
	assert!(matches!(
		window.read_exact_at(ByteOffset(2), &mut bytes),
		Err(WindowError::Bounds(RangeError::OutsideSource))
	));
	assert!(window.read_exact_at(ByteOffset(2), &mut []).is_ok());
}

#[test]
fn preserves_parent_failure() {
	struct FailingSource;
	impl ReadAt for FailingSource {
		type Error = &'static str;
		fn len(&self) -> ByteLen {
			ByteLen(1)
		}
		fn read_exact_at(&self, _: ByteOffset, _: &mut [u8]) -> Result<(), Self::Error> {
			Err("source changed")
		}
	}
	let window = Window::new(FailingSource, ByteOffset(0), ByteLen(1)).unwrap();
	assert!(matches!(
		window.read_exact_at(ByteOffset(0), &mut [0]),
		Err(WindowError::Source("source changed"))
	));
}

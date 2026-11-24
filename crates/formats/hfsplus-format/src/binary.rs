use crate::Error;

pub(crate) fn field<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N], Error> {
	bytes
		.get(offset..)
		.and_then(|remaining| remaining.first_chunk())
		.copied()
		.ok_or(Error::Truncated)
}

pub(crate) fn u16_at(bytes: &[u8], offset: usize) -> Result<u16, Error> {
	field(bytes, offset).map(u16::from_be_bytes)
}

pub(crate) fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, Error> {
	field(bytes, offset).map(u32::from_be_bytes)
}

pub(crate) fn u64_at(bytes: &[u8], offset: usize) -> Result<u64, Error> {
	field(bytes, offset).map(u64::from_be_bytes)
}

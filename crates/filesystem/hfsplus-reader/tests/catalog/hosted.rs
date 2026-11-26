use std::{
	cell::RefCell,
	fs::File,
	io::{self, Read, Seek, SeekFrom},
	path::Path,
};

use hfsplus_core::{ByteLen, ByteOffset, ByteRange, ReadAt};

/// Read-only adapter for committed fixtures kept immutable for the test.
pub(super) struct FixtureFile {
	file: RefCell<File>,
	len: ByteLen,
}

impl FixtureFile {
	pub(super) fn open(path: &Path) -> io::Result<Self> {
		let file = File::open(path)?;
		let len = ByteLen(file.metadata()?.len());
		Ok(Self {
			file: RefCell::new(file),
			len,
		})
	}
}

impl ReadAt for FixtureFile {
	type Error = io::Error;

	fn len(&self) -> ByteLen {
		self.len
	}

	fn read_exact_at(&self, offset: ByteOffset, destination: &mut [u8]) -> io::Result<()> {
		let range =
			ByteRange::new(offset, destination.len().into(), self.len).map_err(io::Error::other)?;
		let mut file = self.file.borrow_mut();
		file.seek(SeekFrom::Start(range.start().0))?;
		file.read_exact(destination)
	}
}

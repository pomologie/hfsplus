use core::ops::Range;

use hfsplus_core::{ByteOffset, ReadAt};
use hfsplus_format::{
	journal::{Activity, HEADER_PREFIX_BYTES, INFO_PREFIX_BYTES, Info},
	volume::Header,
};

use crate::{Error, ForkReadSupport, fork::within_data_area, read_source};

const JOURNALED_MOUNT_VERSION: [u8; 4] = *b"HFSJ";

pub(crate) fn probe<S: ReadAt>(
	source: &S,
	header: &Header,
) -> Result<ForkReadSupport, Error<S::Error>> {
	let geometry = header.geometry;
	let Some(info_block) = header.journal_info else {
		return Ok(ForkReadSupport::UnverifiedJournal);
	};
	// Apple treats other last-mount versions as potentially stale journal pointers.
	if header.last_mounted != JOURNALED_MOUNT_VERSION {
		return Ok(ForkReadSupport::UnverifiedJournal);
	}
	let info_range = geometry.block_range(info_block, 1).map_err(Error::Format)?;
	if !within_data_area(&info_range, geometry) || overlaps_metadata(&info_range, header) {
		return Ok(ForkReadSupport::UnverifiedJournal);
	}
	let mut bytes = [0; INFO_PREFIX_BYTES];
	read_source(source, ByteOffset(info_range.start), &mut bytes)?;
	let Ok(info) = Info::decode(&bytes) else {
		return Ok(ForkReadSupport::UnverifiedJournal);
	};
	let block_bytes = u64::from(geometry.block_size());
	let Some(journal_end) = info.offset.checked_add(info.size) else {
		return Ok(ForkReadSupport::UnverifiedJournal);
	};
	let journal_range = info.offset..journal_end;
	if info.size == 0
		|| !info.offset.is_multiple_of(block_bytes)
		|| !info.size.is_multiple_of(block_bytes)
		|| !within_data_area(&journal_range, geometry)
		|| overlaps(&info_range, &journal_range)
		|| overlaps_metadata(&journal_range, header)
	{
		return Ok(ForkReadSupport::UnverifiedJournal);
	}
	let mut bytes = [0; HEADER_PREFIX_BYTES];
	read_source(source, ByteOffset(journal_range.start), &mut bytes)?;
	Ok(match Activity::decode(&bytes, info) {
		Ok(Activity::Empty) => ForkReadSupport::Supported,
		Ok(Activity::Pending) => ForkReadSupport::NeedsJournalReplay,
		Err(_) => ForkReadSupport::UnverifiedJournal,
	})
}

fn overlaps_metadata(range: &Range<u64>, header: &Header) -> bool {
	let forks = &header.forks;
	[
		&forks.allocation,
		&forks.extents,
		&forks.catalog,
		&forks.attributes,
		&forks.startup,
	]
	.into_iter()
	.any(|fork| {
		// This bounded probe cannot establish where unresolved metadata lives.
		fork.has_overflow()
			|| fork.extents().any(|extent| {
				header
					.geometry
					.block_range(extent.start, extent.blocks)
					.map_or(true, |extent_range| overlaps(range, &extent_range))
			})
	})
}

fn overlaps(first: &Range<u64>, second: &Range<u64>) -> bool {
	first.start < second.end && second.start < first.end
}

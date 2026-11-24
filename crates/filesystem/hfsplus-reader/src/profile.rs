use core::fmt;

use hfsplus_format::volume::{
	CLEAN_UNMOUNT_ATTRIBUTE, CNID_REUSE_ATTRIBUTE, HARDWARE_LOCK_ATTRIBUTE, Header,
	INCONSISTENT_ATTRIBUTE, JOURNALED_ATTRIBUTE, NO_CACHE_ATTRIBUTE, SOFTWARE_LOCK_ATTRIBUTE,
	SPARED_BLOCKS_ATTRIBUTE, UNUSED_NODE_FIX_ATTRIBUTE,
};

const KNOWN_ATTRIBUTES: u32 = CLEAN_UNMOUNT_ATTRIBUTE
	| CNID_REUSE_ATTRIBUTE
	| HARDWARE_LOCK_ATTRIBUTE
	| INCONSISTENT_ATTRIBUTE
	| JOURNALED_ATTRIBUTE
	| NO_CACHE_ATTRIBUTE
	| SOFTWARE_LOCK_ATTRIBUTE
	| SPARED_BLOCKS_ATTRIBUTE
	| UNUSED_NODE_FIX_ATTRIBUTE;

/// Whether the embedded-fork reader can operate on the volume's primary state.
/// This describes the read profile and does not certify filesystem integrity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ForkReadSupport {
	/// The volume declares a clean, unjournaled state and recognized attributes.
	Supported,
	/// A nonjournaled volume requires a consistency check before logical reads.
	Dirty,
	/// The journal may require replay before logical reads.
	NeedsJournalReplay,
	/// The journal must be independently checked for pending transactions.
	UnverifiedJournal,
	/// Bad-block records must be checked before logical reads.
	SparedBlocks,
	/// The active profile does not admit these volume attribute bits.
	UnqualifiedAttributes(u32),
}

impl fmt::Display for ForkReadSupport {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match *self {
			Self::Supported => formatter.write_str("volume supports embedded fork reads"),
			Self::Dirty => formatter.write_str("volume requires a consistency check"),
			Self::NeedsJournalReplay => formatter.write_str("volume journal may require replay"),
			Self::UnverifiedJournal => formatter.write_str("volume journal requires validation"),
			Self::SparedBlocks => formatter.write_str("volume has unverified bad-block records"),
			Self::UnqualifiedAttributes(bits) => {
				write!(
					formatter,
					"volume has unqualified attribute bits {bits:#010x}"
				)
			}
		}
	}
}

pub(crate) fn fork_read_support(header: &Header) -> ForkReadSupport {
	let dirty = !header.unmounted_cleanly() || header.attributes & INCONSISTENT_ATTRIBUTE != 0;
	if header.journal_info.is_some() {
		return if dirty {
			ForkReadSupport::NeedsJournalReplay
		} else {
			ForkReadSupport::UnverifiedJournal
		};
	}
	if dirty {
		return ForkReadSupport::Dirty;
	}
	if header.attributes & SPARED_BLOCKS_ATTRIBUTE != 0 {
		return ForkReadSupport::SparedBlocks;
	}
	match header.attributes & !KNOWN_ATTRIBUTES {
		0 => ForkReadSupport::Supported,
		bits => ForkReadSupport::UnqualifiedAttributes(bits),
	}
}

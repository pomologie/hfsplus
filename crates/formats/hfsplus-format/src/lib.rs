//! Checked HFS+ and HFSX binary format codecs. This crate does not perform I/O.
#![no_std]

pub mod attributes;
mod binary;
pub mod btree;
pub mod catalog;
pub mod hard_links;
pub mod journal;
pub mod volume;

/// A failed HFS+ format admission check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
	/// A field or structure ends prematurely.
	Truncated,
	/// The header does not identify HFS+ or HFSX.
	InvalidSignature,
	/// A recognizable format version is outside this codec's profile.
	UnsupportedVersion(u16),
	/// Allocation geometry exceeds the bounded volume or is internally inconsistent.
	InvalidGeometry,
	/// A fork's allocation, extent sequence or logical length is inconsistent.
	InvalidFork,
	/// A node's descriptor or record offsets are inconsistent.
	InvalidNode,
	/// A catalog key, file, folder or thread record is structurally invalid.
	InvalidCatalog,
	/// An unknown catalog record type.
	UnsupportedCatalogRecord(u16),
	/// An attribute key or inline value is structurally invalid.
	InvalidAttribute,
	/// An attribute uses a non-inline or unknown record tag.
	UnsupportedAttributeRecord(u32),
	/// An inline attribute exceeds the qualified native size limit.
	UnsupportedAttributeSize(usize),
	/// Interpreting a compressed file attribute requires a separate profile.
	UnsupportedCompressedAttribute,
	/// The journal header's signature, checksum or geometry is invalid.
	InvalidJournalHeader,
	/// The journal is uninitialized, is external, or uses unknown flags.
	UnsupportedJournalFlags(u32),
}

impl core::fmt::Display for Error {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		match self {
			Self::Truncated => f.write_str("truncated HFS+ structure"),
			Self::InvalidSignature => f.write_str("invalid HFS+ volume signature"),
			Self::UnsupportedVersion(version) => write!(f, "unsupported HFS+ version {version}"),
			Self::InvalidGeometry => f.write_str("invalid HFS+ volume geometry"),
			Self::InvalidFork => f.write_str("invalid HFS+ fork extent data"),
			Self::InvalidNode => f.write_str("invalid HFS+ B-tree node"),
			Self::InvalidCatalog => f.write_str("invalid HFS+ catalog record"),
			Self::UnsupportedCatalogRecord(kind) => {
				write!(f, "unsupported HFS+ catalog record {kind:#06x}")
			}
			Self::InvalidAttribute => f.write_str("invalid HFS+ attribute record"),
			Self::UnsupportedAttributeRecord(kind) => {
				write!(f, "unsupported HFS+ attribute record {kind:#010x}")
			}
			Self::UnsupportedAttributeSize(bytes) => {
				write!(f, "unsupported HFS+ inline attribute size {bytes} bytes")
			}
			Self::UnsupportedCompressedAttribute => {
				f.write_str("unsupported HFS+ compressed file attribute")
			}
			Self::InvalidJournalHeader => f.write_str("invalid HFS+ journal header"),
			Self::UnsupportedJournalFlags(flags) => {
				write!(f, "unsupported HFS+ journal flags {flags:#010x}")
			}
		}
	}
}

impl core::error::Error for Error {}

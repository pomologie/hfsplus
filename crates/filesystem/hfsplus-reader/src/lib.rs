//! Portable reads from a caller-bounded HFS+ or HFSX volume.
//!
//! The reader opens volume headers, maps embedded and overflow extents, and
//! traverses supported catalog and attribute trees over stored bytes.
#![no_std]

pub mod allocation;
pub mod attributes;
pub mod catalog;
pub mod check;
pub mod decmpfs;
mod fork;
mod journal;
mod memory;
pub mod overflow;
mod profile;
mod volume;

pub use fork::{ForkReader, SpecialFile};
pub(crate) use memory::alloc;
pub use profile::ForkReadSupport;
pub use volume::{AlternateHeader, Error, Volume};

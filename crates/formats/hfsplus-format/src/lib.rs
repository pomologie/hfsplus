//! Checked HFS+ and HFSX binary format codecs. This crate does not perform I/O.
#![no_std]

pub mod attributes;
mod binary;
pub mod bsd;
pub mod btree;
pub mod catalog;
pub mod date;
mod error;
pub mod hard_links;
pub mod journal;
pub mod symlink;
pub mod volume;

pub use error::Error;

#[cfg(kani)]
#[path = "../verification/kani/volume_geometry.rs"]
mod volume_geometry_proofs;

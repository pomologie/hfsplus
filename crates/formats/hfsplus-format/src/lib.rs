//! Checked HFS+ and HFSX binary format codecs. This crate does not perform I/O.
#![no_std]

pub mod attributes;
mod binary;
pub mod btree;
pub mod catalog;
mod error;
pub mod hard_links;
pub mod journal;
pub mod volume;

pub use error::Error;

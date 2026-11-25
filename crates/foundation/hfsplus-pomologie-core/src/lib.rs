//! Portable, exact positional reads for bounded filesystem images.
#![no_std]

mod byte_range;
pub mod names;
mod read_at;
mod window;

pub use byte_range::{ByteLen, ByteOffset, ByteRange, RangeError};
pub use read_at::ReadAt;
pub use window::{Window, WindowError};

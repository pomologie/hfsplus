//! Sealed, byte-addressable image layouts for deterministic HFS+ construction.
//!
//! The layout layer joins filesystem metadata and immutable source windows. It
//! deliberately has no host path, mount, or image-container dependencies.

pub mod allocation;
mod error;
mod plan;
mod stream;
pub mod tree;
pub mod volume;

pub use error::{PlanError, ReadError, WriteError};
pub use plan::{Limits, PlanBuilder, RangeKind, SealedPlan};
pub use stream::ImageReader;

#[cfg(kani)]
#[path = "../verification/kani/bitmap.rs"]
mod bitmap_proof;
#[cfg(kani)]
#[path = "../verification/kani/placement.rs"]
mod placement_proof;

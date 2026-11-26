# HFS+ format verification

H-FMT-01-T03 passed on macOS 14.7.2 arm64 (23H311) with Kani 0.68.0 and CBMC 6.11.0. Both production geometry harnesses verified with zero failures and all six cover properties satisfied.

The first harness calls `Geometry::new` with arbitrary on-disk `u32` block sizes and counts and arbitrary `u64` device lengths. It compares admission against a `u128` model and checks the exact allocation span and alternate-header offset. Zero and non-power-of-two block sizes and products beyond the source return `InvalidGeometry`. The `u32` size and count product always fits `u64`. Header decoding calls this allocation-free geometry check before decoding the fork descriptors.

The second harness checks `Geometry::block_range` for every `u32` start and count in an admitted geometry. A widened sum distinguishes valid endpoints from ranges that cross the volume boundary or overflow the `u32` addition. Every admitted byte range stays within its source.

In a disposable checkout, raising the production minimum block size from 512 to 1024 caused the first harness to fail its admission assertion.

Install Kani 0.68.0 and run `cargo kani setup`. Then run `bash crates/formats/hfsplus-format/verification/check.sh` from the repository root. The harnesses cover volume and block-range arithmetic. The broader H-V01 obligation also covers fork arrays, truncated header decoding, and the required property, fuzz, and differential runs. [Harnesses](kani/volume_geometry.rs) · [H-V01](../../../../verification/obligations.json)

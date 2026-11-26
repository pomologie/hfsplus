# HFS+ format verification

H-FMT-01-T03 passed on macOS 14.7.2 arm64 (23H311) with Kani 0.68.0 and CBMC 6.11.0. Both production geometry harnesses verified with zero failures and all six cover properties satisfied.

The first harness calls `Geometry::new` with arbitrary on-disk `u32` block sizes and counts and arbitrary `u64` device lengths. It compares admission against a `u128` model and checks the exact allocation span and alternate-header offset. Zero and non-power-of-two block sizes and products beyond the source return `InvalidGeometry`. The `u32` size and count product always fits `u64`. Header decoding calls this allocation-free geometry check before decoding the fork descriptors.

The second harness checks `Geometry::block_range` for every `u32` start and count in an admitted geometry. A widened sum distinguishes valid endpoints from ranges that cross the volume boundary or overflow the `u32` addition. Every admitted byte range stays within its source.

In a disposable checkout, raising the production minimum block size from 512 to 1024 caused the first harness to fail its admission assertion.

Install Kani 0.68.0 and run `cargo kani setup`. Then run `bash crates/formats/hfsplus-format/verification/check.sh` from the repository root. The harnesses cover volume and block-range arithmetic. The broader H-V01 obligation also covers fork arrays, truncated header decoding, and the required property, fuzz, and differential runs. [Harnesses](kani/volume_geometry.rs) · [H-V01](../../../../verification/obligations.json)

The format tests at `7618ed2` compare ten valid fork descriptors against independently assembled big-endian records. They cover every count from zero to eight embedded extents, preservation of logical extent order when physical offsets are out of order, and allocation beyond eight populated slots. Every shorter prefix of an intact 512-byte native header and its 80-byte catalog fork descriptor returns `Truncated`. The clean branch-coverage replay passed all 196 tests that ran and recorded the same production coverage counters as the previous baseline. The report is in `target/verification-evidence/v0.1.0/7618ed2-critical-coverage/` with SHA-256 `9724f28933fb352b6390d020828a995da7b5f0d2bcc4702b7b0ded87bea4af31`. [Volume codec tests](../tests/encode_volume.rs)

On clean revision `2cef430`, the `format-wire-property` profile checked 171 fork layouts across 512-, 4,096-, and 65,536-byte blocks, and rejected all 592 short prefixes of the retained native header and catalog fork. The layouts rotate physical extent order, vary extent lengths and the final logical block, and exercise overflow at all eight occupied slots. The model also compared 72 extent byte ranges against widened arithmetic. The checker accepted this profile and the two geometry Kani harnesses on the same revision. Fuzz and differential profiles remain open for H-V01.

Run `bash crates/formats/hfsplus-format/verification/check-volume-wire-properties.sh` with Rust 1.99.1. The combined records and transcripts are in `target/verification-evidence/v0.1.0/2cef430-volume-format/`. The bundle SHA-256 is `ada0dec2e7ba700cd4cc8a4ab07d86ca34b248abf157330e8d5a0d453e77a593`. [Release profiles](../../../../verification/release-profile.json)

## Packed record capacity

H-FMT-02-T06 passed on arm64 macOS with Rust 1.99.1. An independent wire ledger reserves the 14-byte descriptor and two trailing 16-bit offsets. At all seven supported node sizes from 512 to 32,768 bytes, a single leaf record fills the remaining space. The test compares the descriptor, record bytes, and both offsets directly before checking the decoder's result. One byte above the limit fails the alignment check, and the first aligned size above the limit fails the capacity check. Both failures leave the destination bytes intact. [Packed-record test](../tests/encode_btree.rs) · [Apple TN1150](https://developer.apple.com/library/archive/technotes/tn/tn1150.html)

All five B-tree encoder tests, strict focused Clippy, and formatting passed. This case covers physical node capacity. H-V02 separately tracks tree-specific record and traversal checks.

## B-tree offset tables

The `btree-offset-kani` profile exercises production `Node::decode` on 512-byte leaf nodes, the smallest admitted size. It fixes the leaf descriptor and uses one to eight records with arbitrary 16-bit wire offsets. An independent model compares acceptance, requires `InvalidNode` for rejected tables, and checks that returned records borrow exactly their declared byte ranges. Cover checks include decreasing offsets, duplicates, an endpoint inside the table, and eight valid records ending at the table boundary.

The second harness compares the production table-start calculation with a widened signed model for every 16-bit descriptor count and all seven admitted node sizes. For oversized tables in the 512-byte node it also calls `Node::decode` with a valid first offset and requires `InvalidNode`. It reaches the first count beyond capacity and the maximum count.

Run `bash crates/formats/hfsplus-format/verification/check-btree-offsets.sh` with Kani 0.68.0 and CBMC 6.11.0. The proof covers offset tables under these bounds. H-FMT-02-T02 still requires its fuzz evidence, and H-V02 additionally tracks the other node and record codecs and comparator behavior. [Harnesses](kani/btree_offsets.rs) · [Release policy](../../../../verification/release-profile.json)

The clean `eb006a3` replay checked 103 valid extent-overflow leaf and index nodes across all seven node sizes. A separate wire model wrote the descriptor, ordered data and resource keys, physical extents or index child IDs, padding, and trailing offsets. Production encoding matched the full node bytes. Decoding returned the exact record slices and ordered typed keys. The run covers the extent tree wire layout within eight records per node. The remaining catalog and attribute record cases and the fuzz and native gates retain their own evidence requirements.

Run `bash crates/formats/hfsplus-format/verification/check-extent-node-model.sh` with Rust 1.99.1. The retained transcript is `target/verification-evidence/v0.1.0/eb006a3-extent-node-model/replay.log`, SHA-256 `0a9c2085d080865e6b6093a851971a0477131251a77e894ba0c69c15cdfddaad`. [Extent node test](../tests/extents.rs)

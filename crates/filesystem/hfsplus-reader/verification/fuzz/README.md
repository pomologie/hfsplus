# HFS+ resolved fork windows under AddressSanitizer

Run `bash crates/filesystem/hfsplus-reader/verification/fuzz/check-fork-windows.sh` from the repository root. The runner uses nightly `2026-11-04`, cargo-fuzz 0.13.2, and AddressSanitizer. Fetch the pinned dependencies in this directory before running offline.

The immutable sources are the two retained, digest-pinned, verified-chain and chain-free hard-link captures. Each fuzzer input supplies only offset and length selectors. The harness reduces them to valid ranges for each known payload, then compares the returned bytes with the corresponding slice. Every callback opens both fork kinds for one representative alias from each of the two hard-link groups in both captures. The sampled ranges include zero-length windows, and one group has an empty resource fork.

The fixed campaign uses four two-byte seed requests, seed 7, 12,000 runs, at most two input bytes, a five-second input timeout, and a 1,024 MiB RSS limit. It exercises resolved stored-fork reads over these captures. It leaves image bytes intact and uses in-memory positional I/O.

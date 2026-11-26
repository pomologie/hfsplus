#!/usr/bin/env bash
set -euo pipefail

verification_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
test_name=positioned_reads_tests::every_small_layout_matches_its_independent_materialized_byte_model
compiler=$(command -v rustc)
cargo --version
"$compiler" --version
RUSTC="$compiler" cargo test --color never --locked --no-default-features --lib \
	--manifest-path "$verification_dir/../Cargo.toml" -- --exact --show-output "$test_name"

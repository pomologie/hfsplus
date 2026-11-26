#!/usr/bin/env bash
set -euo pipefail

verification_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
test_name=independent_block_set_matches_empty_small_large_and_many_file_layouts
compiler=$(command -v rustc)
cargo --version
"$compiler" --version
RUSTC="$compiler" cargo test --color never --locked --no-default-features --test allocation_coverage \
	--manifest-path "$verification_dir/../Cargo.toml" -- --exact --show-output "$test_name"

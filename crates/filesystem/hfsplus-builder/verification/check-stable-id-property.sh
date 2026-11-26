#!/usr/bin/env bash
set -euo pipefail

verification_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
test_name=catalog_ids_and_threads_match_reference_across_parent_closed_trees
compiler=$(command -v rustc)
cargo --version
"$compiler" --version
RUSTC="$compiler" cargo test --color never --locked --no-default-features --test stable_id_property \
	--manifest-path "$verification_dir/../Cargo.toml" -- --exact --show-output "$test_name"

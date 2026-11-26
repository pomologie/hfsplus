#!/usr/bin/env bash
set -euo pipefail

verification_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
compiler=$(command -v rustc)
cargo --version
"$compiler" --version
RUSTC="$compiler" cargo test --color never --locked --offline --no-default-features \
	--manifest-path "$verification_dir/../Cargo.toml" --test extents -- \
	--exact matches_ordered_extent_nodes_against_independent_wire_records --show-output

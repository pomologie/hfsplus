#!/usr/bin/env bash
set -euo pipefail

verification_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
cargo kani --manifest-path "$verification_dir/../Cargo.toml" \
	--harness decode_leaf_offsets_match_bounded_wire_model \
	--harness table_admission_matches_full_width_wire_counts

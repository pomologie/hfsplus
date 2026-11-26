#!/usr/bin/env bash
set -euo pipefail

verification_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
cargo kani --manifest-path "$verification_dir/../Cargo.toml" \
	--harness geometry_matches_widened_volume_model \
	--harness allocation_block_range_matches_widened_volume_model

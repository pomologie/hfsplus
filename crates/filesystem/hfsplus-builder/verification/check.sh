#!/usr/bin/env bash
set -euo pipefail

verification_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
cargo kani --manifest-path "$verification_dir/../Cargo.toml" \
	--harness placements_cover_exactly_the_admitted_prefix
cargo kani --manifest-path "$verification_dir/../Cargo.toml" \
	--harness allocation_matches_

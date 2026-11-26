#!/usr/bin/env bash
set -euo pipefail

verification_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
cargo kani --manifest-path "$verification_dir/../Cargo.toml" --exact \
	--harness bitmap_proof::sector_allocation_matches_bitmap_model \
	--harness bitmap_proof::sector_allocation_matches_transition_model \
	--harness bitmap_proof::standard_allocation_matches_bitmap_model \
	--harness bitmap_proof::standard_allocation_matches_transition_model

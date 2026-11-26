#!/usr/bin/env bash
set -euo pipefail

verification_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
bash "$verification_dir/check-placement.sh"
cargo kani --manifest-path "$verification_dir/../Cargo.toml" \
	--harness allocation_matches_

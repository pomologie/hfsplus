#!/usr/bin/env bash
set -euo pipefail

verification_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
compiler=$(command -v rustc)
cargo --version
"$compiler" --version
RUSTC="$compiler" cargo test --color never --locked --offline --no-default-features \
	--manifest-path "$verification_dir/../Cargo.toml" --test encode_volume -- \
	--exact volume_wire_shapes_match_the_reference_model --show-output

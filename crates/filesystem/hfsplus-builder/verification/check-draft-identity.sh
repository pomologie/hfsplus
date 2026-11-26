#!/usr/bin/env bash
set -euo pipefail

verification_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
test_name=preserves_reference_ids_across_generated_binding_states_and_seal
compiler=$(command -v rustc)
cargo --version
"$compiler" --version
RUSTC="$compiler" cargo test --color never --locked --no-default-features --test draft_identity \
	--manifest-path "$verification_dir/../Cargo.toml" -- --exact --show-output "$test_name"

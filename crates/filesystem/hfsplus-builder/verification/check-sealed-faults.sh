#!/usr/bin/env bash
set -euo pipefail

verification_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
compiler=$(command -v rustc)
cargo --version
"$compiler" --version
RUSTC="$compiler" cargo test --color never --locked --no-default-features --test sealed_plan \
	--manifest-path "$verification_dir/../Cargo.toml" -- --exact \
	preserves_source_faults_sink_faults_and_cancellation \
	reports_a_source_identity_change_after_sealing \
	cancellation_around_the_next_chunk_preserves_only_the_completed_prefix \
	reader_retries_a_failed_range_and_stops_reading_the_source_at_eof

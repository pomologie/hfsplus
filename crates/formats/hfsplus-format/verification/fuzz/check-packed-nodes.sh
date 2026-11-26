#!/usr/bin/env bash
set -euo pipefail

fuzz_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
scratch=$(mktemp -d "${TMPDIR:-/tmp}/hfsplus-packed-node-fuzz.XXXXXXXX")
default_artifacts="$fuzz_dir/artifacts"
default_target_artifacts="$default_artifacts/packed_nodes"
keep_default_artifacts=false
keep_target_artifacts=false
if [[ -d "$default_artifacts" ]]; then
	keep_default_artifacts=true
fi
if [[ -d "$default_target_artifacts" ]]; then
	keep_target_artifacts=true
fi
cleanup() {
	rm -rf -- "$scratch"
	# cargo-fuzz also creates empty default directories with an explicit artifact prefix.
	if ! "$keep_target_artifacts" && [[ -d "$default_target_artifacts" ]]; then
		rmdir -- "$default_target_artifacts" 2>/dev/null || true
	fi
	if ! "$keep_default_artifacts" && [[ -d "$default_artifacts" ]]; then
		rmdir -- "$default_artifacts" 2>/dev/null || true
	fi
}
trap cleanup EXIT
mkdir "$scratch/corpus" "$scratch/artifacts"

python3 -B - "$scratch/corpus" <<'PY'
from pathlib import Path
import sys

corpus = Path(sys.argv[1])
selectors = [
    (0, 0, 0, 0), (1, 6, 1, 255), (2, 3, 8, 72), (3, 0, 255, 255),
    (4, 1, 45, 1), (5, 2, 254, 127), (6, 0, 255, 255),
]
for index, selection in enumerate(selectors):
    (corpus / f"node-size-{index}").write_bytes(bytes(selection))
PY

toolchain=nightly-2026-11-04
nightly_rustc=$(rustup which --toolchain "$toolchain" rustc)
nightly_bin=$(dirname -- "$nightly_rustc")
export PATH="$nightly_bin:$PATH" RUSTC="$nightly_rustc" CARGO_NET_OFFLINE=true
sanitizer=address
seed=7
runs=12000
max_len=4
timeout_seconds=5
rss_limit_mb=1024
rustc --version
cargo fuzz --version
printf 'HFS+ packed node fuzz profile: %s seed=%s runs=%s max_len=%s timeout_s=%s rss_limit_mb=%s\n' \
	"$sanitizer" "$seed" "$runs" "$max_len" "$timeout_seconds" "$rss_limit_mb"
cargo fuzz run --sanitizer "$sanitizer" --fuzz-dir "$fuzz_dir" --target-dir "$scratch/target" \
	packed_nodes "$scratch/corpus" -- "-seed=$seed" "-runs=$runs" "-max_len=$max_len" \
	"-timeout=$timeout_seconds" "-rss_limit_mb=$rss_limit_mb" "-artifact_prefix=$scratch/artifacts/"
test -z "$(find "$scratch/artifacts" -type f -print -quit)"
printf '%s\n' 'HFS+ packed node fuzz artifacts: 0'

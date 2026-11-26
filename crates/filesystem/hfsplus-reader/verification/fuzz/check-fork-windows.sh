#!/usr/bin/env bash
set -euo pipefail

fuzz_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
fixture_dir="$fuzz_dir/../../tests/fixtures"
scratch=$(mktemp -d "${TMPDIR:-/tmp}/hfsplus-fork-window-fuzz.XXXXXXXX")
default_artifacts="$fuzz_dir/artifacts"
default_target_artifacts="$default_artifacts/fork_windows"
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
	# cargo-fuzz also creates empty default directories even with an artifact prefix.
	if ! "$keep_target_artifacts" && [[ -d "$default_target_artifacts" ]]; then
		rmdir -- "$default_target_artifacts" 2>/dev/null || true
	fi
	if ! "$keep_default_artifacts" && [[ -d "$default_artifacts" ]]; then
		rmdir -- "$default_artifacts" 2>/dev/null || true
	fi
}
trap cleanup EXIT
mkdir "$scratch/corpus" "$scratch/artifacts"

python3 -B - "$fixture_dir" "$scratch/corpus" <<'PY'
import hashlib
from pathlib import Path
import sys

fixtures, corpus = map(Path, sys.argv[1:])
digests = {
    "native-chained-hardlinks.bin": "e22a68bd293506a479b24e14ce8851bc26fb914ba4d126e1c4f1e9726455765a",
    "native-chain-free-hardlinks.bin": "02285cd4522a096c8eb4ff217ba2246f0c935f6f298b477d9119eda8ee222403",
}
for name, expected in digests.items():
    actual = hashlib.sha256((fixtures / name).read_bytes()).hexdigest()
    if actual != expected:
        sys.exit(f"unexpected retained capture digest for {name}: {actual}")
    print(f"Retained HFS+ fixture {name} SHA-256 {actual}")

requests = {"empty-start": (0, 0), "full-data": (0, 22), "inner-resource": (6, 12), "empty-eof": (24, 0)}
for name, request in requests.items():
    (corpus / name).write_bytes(bytes(request))
PY

toolchain=nightly-2026-11-04
nightly_rustc=$(rustup which --toolchain "$toolchain" rustc)
nightly_bin=$(dirname -- "$nightly_rustc")
export PATH="$nightly_bin:$PATH" RUSTC="$nightly_rustc" CARGO_NET_OFFLINE=true
sanitizer=address
seed=7
runs=12000
max_len=2
timeout_seconds=5
rss_limit_mb=1024
rustc --version
cargo fuzz --version
printf 'HFS+ fork window fuzz profile: %s seed=%s runs=%s max_len=%s timeout_s=%s rss_limit_mb=%s\n' \
	"$sanitizer" "$seed" "$runs" "$max_len" "$timeout_seconds" "$rss_limit_mb"
cargo fuzz run --sanitizer "$sanitizer" --fuzz-dir "$fuzz_dir" --target-dir "$scratch/target" \
	fork_windows "$scratch/corpus" -- "-seed=$seed" "-runs=$runs" "-max_len=$max_len" \
	"-timeout=$timeout_seconds" "-rss_limit_mb=$rss_limit_mb" "-artifact_prefix=$scratch/artifacts/"
test -z "$(find "$scratch/artifacts" -type f -print -quit)"
printf '%s\n' 'HFS+ fork window fuzz artifacts: 0'

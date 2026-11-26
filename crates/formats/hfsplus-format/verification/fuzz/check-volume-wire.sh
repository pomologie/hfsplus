#!/usr/bin/env bash
set -euo pipefail

fuzz_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
fixture_dir="$fuzz_dir/../../tests/fixtures"
scratch=$(mktemp -d "${TMPDIR:-/tmp}/hfsplus-volume-wire-fuzz.XXXXXXXX")
trap 'rm -rf -- "$scratch"' EXIT
mkdir "$scratch/corpus" "$scratch/artifacts"
cp "$fixture_dir/native-primary.bin" "$fixture_dir/optical-primary.bin" "$scratch/corpus/"
python3 -B - "$scratch/corpus" <<'PY'
from pathlib import Path
import struct
import sys

corpus = Path(sys.argv[1])
primary = (corpus / "native-primary.bin").read_bytes()
geometry_offset = 40
catalog_fork_offset = 272
fork_bytes = 80
size, count = struct.unpack_from(">II", primary, geometry_offset)
(corpus / "geometry.bin").write_bytes(struct.pack(">IIIIQ", size, count, 0, count, size * count))
(corpus / "catalog-fork.bin").write_bytes(primary[catalog_fork_offset:catalog_fork_offset + fork_bytes])
PY

toolchain=nightly-2026-11-04
nightly_bin=$(dirname -- "$(rustup which --toolchain "$toolchain" rustc)")
export PATH="$nightly_bin:$PATH" CARGO_NET_OFFLINE=true
sanitizer=address
seed=7
runs=20000
max_len=512
timeout_seconds=5
rss_limit_mb=1024
rustc --version
cargo fuzz --version
printf 'HFS+ volume fuzz profile: %s seed=%s runs=%s max_len=%s timeout_s=%s rss_limit_mb=%s\n' \
	"$sanitizer" "$seed" "$runs" "$max_len" "$timeout_seconds" "$rss_limit_mb"
cargo fuzz run --sanitizer "$sanitizer" --fuzz-dir "$fuzz_dir" --target-dir "$scratch/target" \
	volume_wire "$scratch/corpus" -- "-seed=$seed" "-runs=$runs" "-max_len=$max_len" \
	"-timeout=$timeout_seconds" "-rss_limit_mb=$rss_limit_mb" "-artifact_prefix=$scratch/artifacts/"
test -z "$(find "$scratch/artifacts" -type f -print -quit)"
printf '%s\n' 'HFS+ volume fuzz artifacts: 0'

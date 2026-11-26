#!/usr/bin/env bash
set -euo pipefail

verification_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
bash "$verification_dir/check-placement.sh"
bash "$verification_dir/check-allocation.sh"

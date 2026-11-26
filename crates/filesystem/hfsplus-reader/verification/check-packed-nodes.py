#!/usr/bin/env python3
"""Check occupied B-tree record ranges and key order on retained native HFS+ fixtures."""

import os
from pathlib import Path
import subprocess
from tempfile import TemporaryDirectory
import tomllib


ROOT = Path(__file__).resolve().parents[4]
TEST = "retained_packed_nodes_match_record_ranges_and_key_order"


def main():
	with (ROOT / "rust-toolchain.toml").open("rb") as config:
		toolchain = tomllib.load(config)["toolchain"]["channel"]
	cargo = ("rustup", "run", toolchain, "cargo")
	compiler = subprocess.check_output(
		["rustup", "which", "--toolchain", toolchain, "rustc"], text=True, cwd=ROOT,
	).strip()
	subprocess.run([*cargo, "--version"], cwd=ROOT, check=True)
	subprocess.run([compiler, "--version"], cwd=ROOT, check=True)
	with TemporaryDirectory(prefix="hfsplus-packed-nodes-") as target:
		environment = os.environ | {"CARGO_TARGET_DIR": target, "RUSTC": compiler}
		subprocess.run(
			[*cargo, "test", "--color", "never", "--locked", "--no-default-features",
			 "-p", "hfsplus-reader", "--test", "packed_native_nodes",
			 "--", "--exact", "--show-output", TEST],
			env=environment, cwd=ROOT, check=True,
		)


if __name__ == "__main__":
	main()

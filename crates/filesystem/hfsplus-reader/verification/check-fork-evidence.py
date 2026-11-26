#!/usr/bin/env python3
"""Run bounded H-V04 fork and metadata evidence."""

import argparse
import hashlib
from pathlib import Path
import os
import platform
import subprocess
import sys
from tempfile import TemporaryDirectory
import tomllib


DIRECTORY = Path(__file__).resolve().parent
ROOT = DIRECTORY.parents[3]
FIXTURES = DIRECTORY.parent / "tests/fixtures"
INTERLEAVED_ORACLE = (
	ROOT / "crates/filesystem/hfsplus-builder/verification/check-canonical-volume.py"
)
FIXTURE_NAMES = (
	"native-overflow.bin",
	"native-hardlink-resolution.json",
	"native-chained-hardlinks.bin",
	"native-chain-free-hardlinks.bin",
)
DIFFERENTIAL_TESTS = (
	("native_overflow", "native_data_and_resource_forks_cross_both_continuations"),
	("hard_links", "native_and_chain_free_aliases_resolve_identity_metadata_and_distinct_forks"),
	("hard_links", "fork_lifetime::native_stored_forks_outlive_the_catalog_lookup"),
)
FORK_WINDOW_TEST = "fork_lifetime::all_native_alias_fork_windows_match_distinct_payloads"
TEST_START = "HFS+ fork test: "
TIMEOUT = 180


def run(*command, environment=None, show=False):
	result = subprocess.run(
		command, cwd=ROOT, env=environment, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
		text=True, encoding="utf-8", timeout=TIMEOUT,
	)
	if show or result.returncode:
		print(result.stdout, end="", flush=True)
	result.check_returncode()
	return result.stdout.strip()


def check_property(cargo, compiler):
	print(run(*cargo, "--version"), flush=True)
	print(run(compiler, "--version"), flush=True)
	with TemporaryDirectory(prefix="hfsplus-fork-windows-") as target:
		environment = os.environ | {"CARGO_TARGET_DIR": target, "RUSTC": compiler}
		run(*cargo, "test", "--color", "never", "--locked", "--no-default-features",
			"-p", "hfsplus-reader", "--test", "hard_links", "--", "--exact", "--show-output",
			FORK_WINDOW_TEST, environment=environment, show=True)


def check_differential(cargo, compiler):
	run(sys.executable, "-B", INTERLEAVED_ORACLE, "--interleaved-forks", show=True)
	for name in FIXTURE_NAMES:
		digest = hashlib.sha256((FIXTURES / name).read_bytes()).hexdigest()
		print(f"Retained HFS+ fixture {name} SHA-256 {digest}", flush=True)
	with TemporaryDirectory(prefix="hfsplus-fork-evidence-") as target:
		environment = os.environ | {"CARGO_TARGET_DIR": target, "RUSTC": compiler}
		for harness, test in DIFFERENTIAL_TESTS:
			print(f"{TEST_START}{test}", flush=True)
			run(*cargo, "test", "--color", "never", "--locked", "--no-default-features",
				"-p", "hfsplus-reader", "--test", harness, "--", "--exact",
				test, environment=environment, show=True)


def check_native(cargo, compiler):
	if sys.platform != "darwin":
		raise ValueError("native HFS+ fork evidence requires macOS")
	print(f"Python {platform.python_version()}", flush=True)
	print(run(*cargo, "--version"), flush=True)
	print(run(compiler, "--version"), flush=True)
	build = run("/usr/bin/sw_vers", "-buildVersion")
	print(f"macOS {platform.mac_ver()[0]} build {build} arch {platform.machine()}", flush=True)
	manifest = DIRECTORY / "native-fork-tree.json"
	print(f"Native HFS+ tree manifest SHA-256 {hashlib.sha256(manifest.read_bytes()).hexdigest()}",
		flush=True)
	run(sys.executable, "-B", DIRECTORY / "check-fragmented-forks.py", "--tree", show=True)


def main():
	parser = argparse.ArgumentParser(description=__doc__)
	methods = {"property": check_property, "differential": check_differential, "native": check_native}
	parser.add_argument("method", choices=methods)
	arguments = parser.parse_args()
	with (ROOT / "rust-toolchain.toml").open("rb") as config:
		toolchain = tomllib.load(config)["toolchain"]["channel"]
	cargo = ("rustup", "run", toolchain, "cargo")
	compiler = run("rustup", "which", "--toolchain", toolchain, "rustc")
	methods[arguments.method](cargo, compiler)


if __name__ == "__main__":
	try:
		main()
	except (OSError, ValueError, subprocess.SubprocessError) as error:
		print(error, file=sys.stderr)
		sys.exit(1)

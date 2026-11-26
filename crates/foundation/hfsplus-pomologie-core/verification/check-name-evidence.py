#!/usr/bin/env python3
"""Run one bounded H-V03 acceptance method against the current source."""

import argparse
from hashlib import sha256
import os
from pathlib import Path
import platform
import subprocess
import sys
from tempfile import TemporaryDirectory
import tomllib
import unicodedata


CRATE = Path(__file__).resolve().parents[1]
ROOT = CRATE.parents[2]
CORE = "hfsplus-pomologie-core"
BUILDER = "hfsplus-builder"
READER = "hfsplus-reader"
TEST_START = "HFS+ name test: "
COMMAND_TIMEOUT = 180
UNICODE_TABLE = CRATE / "src/names/unicode32.rs"
NATIVE_PAIR_FIXTURE = CRATE / "tests/fixtures/native-collation.tsv"
GEORGIAN_PAIR = ("\u10a0", "\u10d0")
MODERN_GEORGIAN_KEYS = ("\u2d00", "\u10d0")
METHOD_TESTS = {
    "property": (
        (CORE, "names", "historical_name_corpus_has_idempotent_keys_and_all_ordered_triples_obey_the_laws"),
        (CORE, "names", "generated_unicode_triples_preserve_comparison_laws_and_repeated_keys"),
        (BUILDER, "unicode_volume", "preserves_display_units_and_catalog_order_with_stable_ids"),
        (READER, "catalog", "separators::exact_index_separators_follow_the_first_leaf_record"),
    ),
    "differential": (
        (CORE, "names", "folded_comparison_matches_native_equivalence_and_order"),
        (CORE, "names", "decomposed_names_match_native_hfsplus_catalogs_for_shared_unicode_cases"),
        (CORE, "names", "preserves_version_boundary_exposed_by_newer_optical_oracle"),
        (BUILDER, "unicode_volume", "rejects_the_historical_georgian_collision"),
    ),
    "native": (
        (CORE, "names", "kernel::mounted_names_match_normalization_collisions_bounds_and_order"),
    ),
}


def run(*command, environment=None, show=False):
    result = subprocess.run(
        command, cwd=ROOT, env=environment, stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT, text=True, encoding="utf-8", timeout=COMMAND_TIMEOUT,
    )
    if show or result.returncode:
        print(result.stdout, end="", flush=True)
    result.check_returncode()
    return result.stdout.strip()


def differential():
    run(sys.executable, "-B", CRATE / "tools/generate_unicode32.py", "--check")
    print(f"Unicode 3.2 table SHA-256: {sha256(UNICODE_TABLE.read_bytes()).hexdigest()}", flush=True)
    run(sys.executable, "-B", CRATE / "tools/capture_native_collation.py", "--check", show=True)
    pairs = NATIVE_PAIR_FIXTURE.read_bytes().splitlines()
    if b"10a0\t10d0\t0" not in pairs or b"10a0\t2d00\t-1" not in pairs:
        raise ValueError("the live HFS+ Georgian comparison fixture differs")
    modern = tuple(unicodedata.normalize("NFKC", value.casefold()) for value in GEORGIAN_PAIR)
    if modern != MODERN_GEORGIAN_KEYS:
        raise ValueError("modern folding did not reproduce the Georgian witness")
    units = " ".join(f"{ord(value):04x}" for value in modern)
    print(f"Modern Unicode {unicodedata.unidata_version} Georgian keys: {units}", flush=True)


def check(method):
    if method != "property" and sys.platform != "darwin":
        raise ValueError("the native name oracles require macOS")
    with (ROOT / "rust-toolchain.toml").open("rb") as config:
        channel = tomllib.load(config)["toolchain"]["channel"]
    cargo = ("rustup", "run", channel, "cargo")
    compiler = run("rustup", "which", "--toolchain", channel, "rustc")
    print(run(*cargo, "--version"), flush=True)
    print(run(compiler, "--version"), flush=True)
    if method != "property":
        print(f"Python {platform.python_version()}", flush=True)
        build = run("/usr/bin/sw_vers", "-buildVersion")
        print(f"macOS {platform.mac_ver()[0]} build {build} arch {platform.machine()}", flush=True)
    if method == "differential":
        differential()
    if method == "native":
        run(sys.executable, "-B", CRATE / "tools/capture_kernel_names.py", "--check", show=True)
    with TemporaryDirectory(prefix="hfsplus-name-evidence-") as temporary:
        target = Path(temporary) / "target"
        environment = os.environ | {"RUSTC": compiler, "CARGO_TARGET_DIR": str(target)}
        for package, target_name, test in METHOD_TESTS[method]:
            print(f"{TEST_START}{test}", flush=True)
            run(*cargo, "test", "--color", "never", "--locked", "--no-default-features", "-p", package,
                "--test", target_name, "--", "--exact", "--show-output", test,
                environment=environment, show=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("method", choices=METHOD_TESTS)
    arguments = parser.parse_args()
    try:
        check(arguments.method)
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print(error, file=sys.stderr)
        sys.exit(1)

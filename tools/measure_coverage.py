#!/usr/bin/env python3
"""Collect line and branch counts from the owned HFS+ production sources."""

# Adapted from Pomologie APFS commit 04a755c (Apache-2.0).

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile


TOOLCHAIN = "nightly-2026-11-04"
NEXTEST_VERSION = "0.9.151"
COVERAGE_FLAGS = "-C instrument-coverage -Z coverage-options=branch"
CRITICAL_SOURCES = {
    "crates/foundation/hfsplus-pomologie-core/src/names.rs",
    "crates/foundation/hfsplus-pomologie-core/src/names/collation.rs",
    "crates/formats/hfsplus-format/src/btree.rs",
    "crates/formats/hfsplus-format/src/volume.rs",
    "crates/filesystem/hfsplus-builder/src/allocation.rs",
    "crates/filesystem/hfsplus-builder/src/plan.rs",
    "crates/filesystem/hfsplus-builder/src/stream.rs",
    "crates/filesystem/hfsplus-reader/src/allocation.rs",
}


def command(*args, cwd=None, env=None):
    run = subprocess.run(args, cwd=cwd, env=env, text=True, capture_output=True, check=False)
    if run.returncode:
        raise ValueError(f"{args[0]} exited with {run.returncode}\n{run.stdout}{run.stderr}")
    return run.stdout


def test_objects(artifacts):
    objects = set()
    for line in artifacts.splitlines():
        item = json.loads(line)
        if item["reason"] == "compiler-artifact" and item["executable"] and item["profile"]["test"]:
            objects.add(item["executable"])
    if not objects:
        raise ValueError("Cargo built no instrumented test binaries")
    return sorted(objects)


def coverage_rows(root, export):
    document = json.loads(export)
    if document["type"] != "llvm.coverage.json.export" or len(document["data"]) != 1:
        raise ValueError("expected one LLVM coverage report")
    tracked = set(command("git", "ls-files", "-z", cwd=root).split("\0"))
    owned = {}
    for item in document["data"][0]["files"]:
        path = (root / item["filename"]).resolve()
        if not path.is_relative_to(root):
            continue
        relative = path.relative_to(root)
        if (relative.parts[0] != "crates" or "src" not in relative.parts
                or "tests" in relative.parts or relative.name == "tests.rs"):
            continue
        name = relative.as_posix()
        if name not in tracked or name in owned:
            raise ValueError(f"unexpected owned source path: {name}")
        summary = item["summary"]
        owned[name] = {
            "file": name, "source_sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            "lines": summary["lines"], "branches": summary["branches"],
        }
    missing = CRITICAL_SOURCES - owned.keys()
    if missing:
        raise ValueError(f"LLVM report omitted critical sources: {', '.join(sorted(missing))}")
    return [owned[name] for name in sorted(owned)]


def measure(root, output):
    status = ("status", "--porcelain", "--untracked-files=normal", "--ignore-submodules=none")
    if command("git", *status, cwd=root) or output.exists():
        raise ValueError("coverage requires a clean candidate and a new output directory")
    commit = command("git", "rev-parse", "HEAD", cwd=root).strip()
    rustc = Path(command("rustup", "which", "--toolchain", TOOLCHAIN, "rustc").strip())
    sysroot = Path(command(rustc, "--print", "sysroot").strip())
    host = next(line.split()[1] for line in command(rustc, "-vV").splitlines() if line.startswith("host:"))
    suffix = ".exe" if os.name == "nt" else ""
    tools = sysroot / "lib/rustlib" / host / "bin"
    cargo = rustc.with_name(f"cargo{suffix}")
    llvm_cov = tools / f"llvm-cov{suffix}"
    llvm_profdata = tools / f"llvm-profdata{suffix}"

    with tempfile.TemporaryDirectory(prefix="hfsplus-measure-coverage-") as directory:
        scratch = Path(directory)
        profiles = scratch / "profiles"
        profiles.mkdir()
        build_profiles = scratch / "build-profiles"
        build_profiles.mkdir()
        env = os.environ | {
            "PATH": str(rustc.parent) + os.pathsep + os.environ["PATH"],
            "RUSTUP_TOOLCHAIN": TOOLCHAIN,
            "RUSTC": str(rustc),
            "RUSTFLAGS": COVERAGE_FLAGS,
            "CARGO_INCREMENTAL": "0",
            "CARGO_TARGET_DIR": str(scratch / "target"),
            "LLVM_PROFILE_FILE": str(profiles / "%m-%p.profraw"),
        }
        env.pop("CARGO_ENCODED_RUSTFLAGS", None)
        version = command(cargo, "nextest", "--version", env=env).splitlines()[0]
        if version.split()[1] != NEXTEST_VERSION:
            raise ValueError(f"coverage requires Nextest {NEXTEST_VERSION}, found {version}")
        options = ("--locked", "--workspace", "--no-default-features")
        build_env = env | {"LLVM_PROFILE_FILE": str(build_profiles / "%m-%p.profraw")}
        objects = test_objects(command(cargo, "test", *options, "--no-run", "--message-format=json",
                                       cwd=root, env=build_env))
        # Nextest reports the executed test summary on stderr.
        run = subprocess.run([cargo, "nextest", "run", *options, "--status-level", "fail"],
                             cwd=root, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, check=False)
        if run.returncode:
            raise ValueError(run.stdout)
        raw = sorted(profiles.glob("*.profraw"))
        if not raw:
            raise ValueError("Nextest produced no coverage profiles")
        merged = scratch / "tests.profdata"
        command(llvm_profdata, "merge", "-sparse", *raw, "-o", merged)
        arguments = [objects[0], f"-instr-profile={merged}", "-summary-only"]
        for binary in objects[1:]:
            arguments.extend(("-object", binary))
        rows = coverage_rows(root, command(llvm_cov, "export", *arguments, cwd=root))
        report = {
            "commit": commit, "host": host, "toolchain": TOOLCHAIN,
            "rustc": command(rustc, "--version").strip(), "nextest": version,
            "llvm_cov": command(llvm_cov, "--version").strip(), "rustflags": COVERAGE_FLAGS,
            "instrumented_binaries": len(objects), "raw_profiles": len(raw),
            "excluded_build_profiles": len(list(build_profiles.glob("*.profraw"))),
            "nextest_log_sha256": hashlib.sha256(run.stdout.encode()).hexdigest(),
            "owned_sources": rows,
        }
        if command("git", *status, cwd=root) or command("git", "rev-parse", "HEAD", cwd=root).strip() != commit:
            raise ValueError("candidate changed during coverage measurement")
        output.mkdir(parents=True)
        (output / "coverage.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        (output / "nextest.log").write_text(run.stdout, encoding="utf-8")
        print(f"Collected HFS+ coverage at {commit} from {len(objects)} test binaries")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    try:
        measure(Path(__file__).resolve().parents[1], arguments.output.resolve())
    except (KeyError, OSError, ValueError) as error:
        parser.exit(1, f"{parser.prog}: {error}\n")


if __name__ == "__main__":
    main()

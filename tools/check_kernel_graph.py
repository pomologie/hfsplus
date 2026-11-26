"""Check the normal Cargo dependency graph of a bare-metal release profile."""

import argparse
from pathlib import Path
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
CORE = "hfsplus-pomologie-core"
FORMAT = "hfsplus-format"
READER = "hfsplus-reader"
TARGETS = ("aarch64-unknown-none", "x86_64-unknown-none")
KERNEL_PACKAGES = {
    CORE: (CORE,),
    FORMAT: (FORMAT,),
    READER: (CORE, FORMAT, READER),
}


def target_graph(root, package, target, features=(), locked=True):
    command = [
        "cargo", "tree", "--target", target, "-p", package, "--no-default-features",
        "--edges", "normal,no-proc-macro", "--no-dedupe", "--prefix", "none",
        "--format", "{p}\t{f}",
    ]
    if locked:
        command.append("--locked")
    if features:
        command.extend(("--features", ",".join(features)))
    output = subprocess.check_output(command, cwd=root, text=True)
    graph = {}
    for line in output.splitlines():
        description, feature_list = line.split("\t", 1)
        name = description.split(maxsplit=1)[0]
        graph.setdefault(name, set()).update(filter(None, feature_list.split(",")))
    return graph


def enforce(graph, expected):
    missing = expected.keys() - graph.keys()
    unexpected = graph.keys() - expected.keys()
    feature_changes = {
        name: sorted(graph[name] ^ expected[name])
        for name in sorted(graph.keys() & expected.keys())
        if graph[name] != expected[name]
    }
    if missing or unexpected or feature_changes:
        raise ValueError(
            f"kernel dependency boundary differs (missing: {sorted(missing)}, "
            f"unexpected: {sorted(unexpected)}, changed features: {feature_changes})"
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", choices=KERNEL_PACKAGES, required=True)
    parser.add_argument("--target", choices=TARGETS, required=True)
    options = parser.parse_args()
    graph = target_graph(ROOT, options.package, options.target)
    try:
        enforce(graph, {package: set() for package in KERNEL_PACKAGES[options.package]})
    except ValueError as error:
        print(error, file=sys.stderr)
        return 1
    print(f"{options.package} on {options.target}: {', '.join(sorted(graph))}; no enabled features")
    return 0


if __name__ == "__main__":
    sys.exit(main())

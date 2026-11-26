"""Validate the exact H-V04 fork and metadata transcripts."""

import re

from hfsplus_evidence_transcript import (
    DIFFERENTIAL_LINE_COUNT, require, verify_address_sanitizer, verify_interleaved_differential,
    verify_rust_tests, verify_tool_version,
)


TEST_START = "HFS+ fork test: "
DIFFERENTIAL_TESTS = (
    "native_data_and_resource_forks_cross_both_continuations",
    "native_and_chain_free_aliases_resolve_identity_metadata_and_distinct_forks",
    "fork_lifetime::native_stored_forks_outlive_the_catalog_lookup",
)
NATIVE_TEST = "tree::native_nested_tree_matches_mounted_entries"
NO_WRITE_OUTPUT = re.compile(r"\*\* /dev/(r?)disk(\d+) \(NO WRITE\)")
NATIVE_RAW_DIGEST = re.compile(r"Native raw image SHA-256 [0-9a-f]{64}")


def verify_exact_test(lines, versions, profile, name, report):
    test_profile = profile | {"harnesses": {name: 1}}
    output, results, summary = verify_rust_tests("\n".join(versions + lines), test_profile)
    require(not any(output[summary + 1:]), f"unexpected output after {name}")
    observed = [line for line in output[results[0] + 1:summary] if line]
    require(observed == report, f"incorrect scope or output for {name}")


def verify_fork_differential(transcript, profile):
    expected_harnesses = {"interleaved-forks": 1} | dict.fromkeys(DIFFERENTIAL_TESTS, 1)
    require(profile["harnesses"] == expected_harnesses, "wrong H-V04 differential harnesses")
    lines = transcript.splitlines()
    verify_interleaved_differential("\n".join(lines[:DIFFERENTIAL_LINE_COUNT]), profile)
    starts = [index for index, line in enumerate(lines) if line.startswith(TEST_START)]
    require([lines[index].removeprefix(TEST_START) for index in starts] == list(DIFFERENTIAL_TESTS),
            "incorrect H-V04 differential invocations")
    expected_fixtures = [f"Retained HFS+ fixture {name} SHA-256 {digest}"
                         for name, digest in profile["bounds"]["retained_fixtures"].items()]
    require(lines[DIFFERENTIAL_LINE_COUNT:starts[0]] == expected_fixtures,
            "incorrect retained HFS+ fixture digests")
    ends = [*starts[1:], len(lines)]
    for name, start, end in zip(DIFFERENTIAL_TESTS, starts, ends, strict=True):
        verify_exact_test(lines[start + 1:end], lines[1:3], profile, name, [])


def verify_no_write_check(lines, version, volume):
    if lines and lines[0].startswith("Can't open /dev/rdisk"):
        denial, *lines = lines
    else:
        denial = None
    match = NO_WRITE_OUTPUT.fullmatch(lines[0]) if lines else None
    require(match and (denial is None or denial == f"Can't open /dev/rdisk{match[2]}: Permission denied"),
            "incorrect Apple no-write device")
    expected = [
        f"   Executing fsck_hfs (version hfs-{version}).",
        "** Checking non-journaled HFS Plus Volume.",
        f"   The volume name is {volume}",
        "** Checking extents overflow file.",
        "** Checking catalog file.",
        "** Checking multi-linked files.",
        "** Checking catalog hierarchy.",
        "** Checking extended attributes file.",
        "** Checking volume bitmap.",
        "** Checking volume information.",
        f"** The volume {volume} appears to be OK.",
    ]
    require(lines[1:] == expected, "incomplete Apple no-write filesystem check")


def verify_fork_native(transcript, profile):
    require(profile["harnesses"] == {NATIVE_TEST: 1}, "wrong H-V04 native harness")
    lines = transcript.splitlines()
    versions = profile["tool_versions"]
    bounds = profile["bounds"]
    for index, (tool, key) in enumerate((("Python", "python"), ("cargo", "cargo"), ("rustc", "rustc"))):
        verify_tool_version(lines[index:index + 1], tool, versions[key])
    host = f"macOS {versions['macos']} build {versions['macos_build']} arch {bounds['macos_arch']}"
    manifest = f"Native HFS+ tree manifest SHA-256 {bounds['fixture_sha256']}"
    require(lines[3:5] == [host, manifest], "incorrect H-V04 native host or pinned manifest")
    detach = [index for index, line in enumerate(lines) if line == "Apple no-write check detached"]
    require(len(detach) == 1 and detach[0] >= 5, "missing detach after Apple no-write check")
    verify_no_write_check(lines[5:detach[0]], versions["fsck_hfs"], bounds["volume_label"])

    first = detach[0] + 1
    expected_digests = [f"Mounted {kind}.bin SHA-256 {fork['sha256']}"
                        for kind, fork in bounds["forks"].items()]
    require(lines[first:first + len(expected_digests)] == expected_digests,
            "incorrect mounted data or resource digest")
    tree = first + len(expected_digests)
    tree_report = (rf"Native mount matched {bounds['stored_names']} stored names, "
                   r"shared inode [1-9][0-9]*, UF_NODUMP, and exact attributes and link target")
    require(len(lines) >= tree + 6 and re.fullmatch(tree_report, lines[tree])
            and lines[tree + 1:tree + 3] == ["Apple read-only mount detached",
                                           f"Native raw image bytes {bounds['raw_image_bytes']}"]
            and NATIVE_RAW_DIGEST.fullmatch(lines[tree + 3])
            and lines[tree + 4] == "Native HFS+ tree matched the pinned paths, attributes, symlink and fork payloads",
            "incorrect mounted metadata, detach, or exported size")
    fork_reports = [f"{kind}.bin: matched {fork['bytes']} native bytes across {fork['extents']} extents"
                    for kind, fork in bounds["forks"].items()]
    report = ["successes:", f"---- {NATIVE_TEST} stdout ----", *(fork_reports * bounds["hard_link_names"]),
              "Matched native tree names, metadata, hard-link identities, attributes and symlink bytes",
              "successes:", f"    {NATIVE_TEST}"]
    verify_exact_test(lines[tree + 5:], lines[1:3], profile, NATIVE_TEST, report)


def verify_fork_windows_fuzz(transcript, profile):
    digests = profile["bounds"]["retained_fixture_sha256"]
    lines = transcript.splitlines()
    retained = [line for line in lines if line.startswith("Retained HFS+ fixture ")]
    expected = [f"Retained HFS+ fixture {name} SHA-256 {digest}" for name, digest in digests.items()]
    require(retained == expected and lines[:len(expected)] == expected,
            "incorrect retained HFS+ fork captures")
    verify_address_sanitizer(transcript, profile, target="fork_windows", report="fork window")


FORK_VALIDATORS = {
    "differential": verify_fork_differential,
    "native": verify_fork_native,
    "fuzz": verify_fork_windows_fuzz,
}

"""Check invocation transcripts under the committed HFS+ evidence policy."""

import hashlib
import json
import re


class EvidenceError(ValueError):
    """Evidence failed a release policy requirement."""


def require(condition, message):
    if not condition:
        raise EvidenceError(message)


KANI_HARNESS = re.compile(r"^Checking harness (\S+)\.\.\.$", re.MULTILINE)
KANI_SUMMARY = "Manual Harness Summary:"
KANI_SUCCESS = "VERIFICATION:- SUCCESSFUL"
KANI_PROPERTIES = re.compile(r"\*\* (\d+) of (\d+) failed(?: \((\d+) unreachable\))?")
KANI_COVERS = re.compile(r"\*\* (\d+) of (\d+) cover properties satisfied")
RUST_TEST_RESULT = re.compile(r"test (\S+) \.\.\. (ok|FAILED|ignored)")
RUST_TEST_SUMMARY = re.compile(
    r"test result: ok\. (\d+) passed; 0 failed; 0 ignored; 0 measured; \d+ filtered out; finished in \d+\.\d+s"
)
PROPERTY_CASES = "HFS+ property cases checked: "
DIFFERENTIAL_LINE_COUNT = 6


def verify_kani(transcript, profile):
    harnesses = profile["harnesses"]
    versions = profile["tool_versions"]
    lines = transcript.splitlines()
    require([line for line in lines if line.startswith("Kani Rust Verifier ")] ==
            [f"Kani Rust Verifier {versions['kani']} (cargo plugin)"],
            "transcript has the wrong Kani version")
    cbmc_versions = {line for line in lines if re.fullmatch(r"CBMC \S+", line)}
    require(cbmc_versions == {f"CBMC {versions['cbmc']}"}, "transcript has the wrong CBMC version")
    results, sep, footer = transcript.partition(KANI_SUMMARY)
    count = len(harnesses)
    summary = f"Complete - {count} successfully verified harnesses, 0 failures, {count} total."
    require(sep and footer.splitlines().count(summary) == 1 and KANI_SUMMARY not in footer,
            "incomplete Kani summary")

    seen = set()
    blocks = KANI_HARNESS.split(results)
    for name, result in zip(blocks[1::2], blocks[2::2], strict=True):
        require(name in harnesses and name not in seen, f"unexpected Kani harness {name}")
        seen.add(name)
        lines = [line.strip() for line in result.splitlines()]
        require(lines.count(KANI_SUCCESS) == 1 and "VERIFICATION:- FAILED" not in lines,
                f"unsuccessful Kani harness {name}")
        properties = [match.groups() for line in lines if (match := KANI_PROPERTIES.fullmatch(line))]
        covers = [match.groups() for line in lines if (match := KANI_COVERS.fullmatch(line))]
        require(len(properties) == 1, f"missing or duplicate property result for {name}")
        failed, total, unreachable = properties[0]
        require(int(failed) == 0 and int(total) > int(unreachable or 0),
                f"failed or vacuous Kani properties for {name}")
        require(len(covers) == 1 and tuple(map(int, covers[0])) == (harnesses[name],) * 2,
                f"missing or unreached Kani covers for {name}")
    require(seen == set(harnesses), "missing required Kani harnesses")


def verify_tool_version(lines, tool, version):
    observed = [line for line in lines if line.startswith(f"{tool} ")]
    pinned = rf"{tool} {re.escape(version)}(?: \([^()]+\))?"
    require(len(observed) == 1 and re.fullmatch(pinned, observed[0]),
            f"transcript has the wrong {tool} version")


def verify_rust_tests(transcript, profile):
    lines = transcript.splitlines()
    harnesses = profile["harnesses"]
    versions = profile["tool_versions"]
    for tool in ("cargo", "rustc"):
        verify_tool_version(lines, tool, versions[tool])
    starts = [index for index, line in enumerate(lines) if re.fullmatch(r"running \d+ tests?", line)]
    results = [(index, match.groups()) for index, line in enumerate(lines)
               if (match := RUST_TEST_RESULT.fullmatch(line))]
    summaries = [index for index, line in enumerate(lines) if line.startswith("test result: ")]
    count = len(harnesses)
    start = f"running {count} test" + ("s" if count != 1 else "")
    summary = RUST_TEST_SUMMARY.fullmatch(lines[summaries[0]]) if len(summaries) == 1 else None
    require(len(starts) == 1 and lines[starts[0]] == start
            and len(results) == count and {result for _, result in results} == {(name, "ok") for name in harnesses}
            and summary and int(summary[1]) == count
            and all(starts[0] < index < summaries[0] for index, _ in results),
            f"missing, unexpected, or unsuccessful exact {profile['method']} test")
    return lines, [index for index, _ in results], summaries[0]


def verify_rust_test_output(transcript, profile, marker, expected, error):
    harnesses = profile["harnesses"]
    require(len(harnesses) == 1, f"{profile['method']} transcript requires one exact test")
    (name, _), = harnesses.items()
    lines, results, summary = verify_rust_tests(transcript, profile)
    headers = [index for index, line in enumerate(lines) if line == f"---- {name} stdout ----"]
    cases = [index for index, line in enumerate(lines) if line.startswith(marker)]
    require(len(headers) == 1 and len(cases) == 1 and cases[0] == headers[0] + 1
            and results[0] < headers[0] < cases[0] < summary and lines[cases[0]] == expected, error)


def verify_property_test(transcript, profile):
    count = next(iter(profile["harnesses"].values()), None)
    verify_rust_test_output(transcript, profile, PROPERTY_CASES, f"{PROPERTY_CASES}{count}",
                            "missing or incorrect property case count")


def verify_draft_state_machine(transcript, profile):
    bounds = profile["bounds"]
    pending, sealed = bounds["unbound_prefixes"], bounds["sealed_histories"]
    marker = "HFS+ draft histories checked: "
    totals = f"{pending + sealed} total, {pending} pending, {sealed} sealed"
    expected = f"{marker}{totals} ({bounds['catalog_objects']} catalog objects)"
    require(list(profile["harnesses"].values()) == [pending + sealed],
            "inconsistent state-machine history bound")
    verify_rust_test_output(transcript, profile, marker, expected, "missing or incorrect draft history counts")


def verify_fault_tests(transcript, profile):
    harnesses = profile["harnesses"]
    require(all(count == 1 for count in harnesses.values()), "fault transcript requires each exact test once")
    verify_rust_tests(transcript, profile)


def verify_volume_wire_differential(transcript, profile):
    lines = transcript.splitlines()
    bounds = profile["bounds"]
    headers = bounds["native_headers"]
    forks = bounds["fork_rows"]
    count = headers + forks
    require(len(lines) == count + 5 and profile["harnesses"] == {"native-volume-wire": count},
            "incomplete native volume wire comparison")
    versions = profile["tool_versions"]
    for tool in ("Python", "cargo", "rustc"):
        verify_tool_version(lines[:3], tool, versions[tool.lower()])
    require(lines[3] == f"Verified native fixture ledgers SHA-256 {bounds['fixture_sha256']}",
            "native volume fixture ledger digest mismatch")
    rows = lines[4:-1]
    digest = hashlib.sha256("\n".join(rows).encode()).hexdigest()
    require(sum(row.startswith("HFS+ header ") for row in rows) == headers
            and sum(row.startswith("HFS+ fork ") for row in rows) == forks
            and digest == bounds["wire_rows_sha256"]
            and lines[-1] == f"Compared {count} native wire rows SHA-256 {digest}",
            "incorrect native volume and fork comparison rows")


def verify_interleaved_differential(transcript, profile):
    lines = transcript.splitlines()
    versions = profile["tool_versions"]
    for tool in ("Python", "cargo", "rustc"):
        verify_tool_version(lines[:3], tool, versions[tool.lower()])
    bounds = profile["bounds"]
    keys = [tuple(key) for key in bounds["continuation_keys"]]
    data, resource = bounds["fork_lengths_bytes"]
    expected = [
        f"Interleaved image {bounds['image_bytes']} bytes SHA-256 {bounds['image_sha256']}",
        f"Independent raw reader found {bounds['catalog_records']} unique catalog records at depth {bounds['catalog_depth']}",
        f"Independent raw reader matched all {len(keys)} continuation keys {keys} "
        f"and both complete forks ({data}, {resource} bytes)",
    ]
    require(lines[3:] == expected, "incorrect interleaved fixture digest, catalog, or complete fork evidence")


def verify_allocation_differential(transcript, profile):
    lines = transcript.splitlines()
    verify_interleaved_differential("\n".join(lines[:DIFFERENTIAL_LINE_COUNT]), profile)
    bounds = profile["bounds"]
    expected = (
        f"Independent allocation matched both headers and bitmap across {bounds['total_blocks']} blocks: "
        f"{bounds['reserved_blocks']} reserved, {bounds['metadata_blocks']} metadata, "
        f"{bounds['file_blocks']} file, {bounds['free_blocks']} free"
    )
    require(lines[DIFFERENTIAL_LINE_COUNT:] == [expected], "incorrect independent allocation comparison")


def verify_frozen_identity(transcript, profile):
    lines = transcript.splitlines()
    versions = profile["tool_versions"]
    for tool in ("Python", "cargo", "rustc"):
        verify_tool_version(lines[:3], tool, versions[tool.lower()])
    bounds = profile["bounds"]
    generated, stubs = tuple(bounds["generated_cnids"]), tuple(bounds["stub_cnids"])
    expected = [
        f"Frozen draft image {bounds['image_bytes']} bytes SHA-256 {bounds['image_sha256']}",
        f"Independent catalog matched {bounds['catalog_objects']} objects and {bounds['catalog_threads']} threads "
        f"at depth {bounds['catalog_depth']}, with root CNID {bounds['root_cnid']} and next CNID {bounds['next_cnid']}",
        f"Independent forks matched generated CNIDs {generated}, hard-link stubs {stubs}, "
        f"and shared inode {bounds['shared_inode']} with reference {bounds['link_reference']}",
    ]
    require(lines[3:] == expected, "incorrect frozen-draft fixture, catalog or identity comparison")


def verify_differential(transcript, profile):
    harnesses = profile["harnesses"]
    require(len(harnesses) == 1, "differential transcript requires one exact oracle")
    (name, count), = harnesses.items()
    oracles = {"interleaved-forks": verify_interleaved_differential,
               "interleaved-allocation": verify_allocation_differential,
               "frozen-draft-catalog": verify_frozen_identity}
    require(count == 1 and name in oracles, "unknown differential oracle")
    oracles[name](transcript, profile)


def verify_interleaved_native(transcript, profile):
    lines = transcript.splitlines()
    verify_interleaved_differential("\n".join(lines[:DIFFERENTIAL_LINE_COUNT]), profile)
    native = lines[DIFFERENTIAL_LINE_COUNT:]
    require(len(native) >= 5, "incomplete Apple no-write transcript")
    attach_argv, attachment_status, fsck_argv, *report = native
    bounds = profile["bounds"]
    require(attach_argv == f"Native attachment argv {json.dumps(bounds['attachment_argv'])}",
            "incorrect read-only software attachment command")
    attachment = re.fullmatch(r"Native attachment checked unmounted (/dev/disk[0-9]+)", attachment_status)
    require(attachment, "missing verified unmounted software attachment")
    device = attachment[1]
    require(fsck_argv == f"Native fsck argv {json.dumps([*bounds['fsck_argv'], device])}",
            "incorrect no-write Apple checker command or target")
    raw_device = device.replace("/dev/disk", "/dev/rdisk")
    if report[0] == f"Can't open {raw_device}: Permission denied":
        report = report[1:]
    version = profile["tool_versions"]["fsck_hfs"]
    volume = bounds["volume_label"]
    expected = [
        f"   Executing fsck_hfs (version hfs-{version}).",
        "** Checking non-journaled HFS Plus Volume.",
        f"   The volume name is {volume}",
        "** Checking extents overflow file.",
        "** Checking catalog file.",
        "** Checking multi-linked files.",
        "** Checking catalog hierarchy.",
        "** Checking volume bitmap.",
        "** Checking volume information.",
        f"** The volume {volume} appears to be OK.",
        f"Native attachment detached {device}",
        f"Native no-write image SHA-256 {bounds['image_sha256']}",
    ]
    require(report and report[0] in {f"** {device} (NO WRITE)", f"** {raw_device} (NO WRITE)"}
            and report[1:] == expected,
            "incorrect Apple no-write result, detach, or fixture digest")


def verify_native_allocation_counts(transcript, profile):
    lines = transcript.splitlines()
    versions = profile["tool_versions"]
    for tool in ("Python", "cargo", "rustc"):
        verify_tool_version(lines[:3], tool, versions[tool.lower()])
    bounds = profile["bounds"]
    digest = f"canonical.hfs {bounds['image_sha256']}"
    expected = [
        f"macOS {versions['macos']} build {versions['macos_build']}",
        digest,
        f"macOS matched {bounds['total_blocks']} total and {bounds['free_blocks']} free "
        f"{bounds['allocation_block_bytes']}-byte allocation blocks",
        "Native read-only allocation attachment detached",
        digest,
    ]
    require(lines[3:] == expected, "incorrect native allocation counts, detach, or fixture digest")


def verify_native_frozen_identity(transcript, profile):
    lines = transcript.splitlines()
    verify_frozen_identity("\n".join(lines[:DIFFERENTIAL_LINE_COUNT]), profile)
    bounds, versions = profile["bounds"], profile["tool_versions"]
    generated = tuple(bounds["generated_cnids"])
    expected = [
        f"macOS {versions['macos']} build {versions['macos_build']}",
        f"Native identity attachment argv {json.dumps(bounds['attachment_argv'])}",
        "Native identity mount verified read-only",
        f"macOS matched root CNID {bounds['root_cnid']}, generated CNIDs {generated} and both forks, "
        f"and shared inode {bounds['shared_inode']} with {len(bounds['stub_cnids'])} links and exact data",
        "Native identity attachment detached",
        f"Native identity image SHA-256 {bounds['image_sha256']}",
    ]
    require(lines[DIFFERENTIAL_LINE_COUNT:] == expected, "incorrect native frozen-draft mount, identities, or digest")


def verify_native(transcript, profile):
    harnesses = profile["harnesses"]
    require(len(harnesses) == 1, "native transcript requires one exact oracle")
    (name, count), = harnesses.items()
    oracles = {"interleaved-forks": verify_interleaved_native,
               "canonical-allocation-counts": verify_native_allocation_counts,
               "frozen-draft-catalog": verify_native_frozen_identity}
    require(count == 1 and name in oracles, "unknown native oracle")
    oracles[name](transcript, profile)

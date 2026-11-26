"""Check invocation transcripts under the committed HFS+ evidence policy."""

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


def verify_property_test(transcript, profile):
    harnesses = profile["harnesses"]
    require(len(harnesses) == 1, "property transcript requires one exact test")
    (name, count), = harnesses.items()
    lines, results, summary = verify_rust_tests(transcript, profile)
    headers = [index for index, line in enumerate(lines) if line == f"---- {name} stdout ----"]
    cases = [index for index, line in enumerate(lines) if line.startswith(PROPERTY_CASES)]
    require(len(headers) == 1 and len(cases) == 1 and cases[0] == headers[0] + 1
            and results[0] < headers[0] < cases[0] < summary
            and lines[cases[0]] == f"{PROPERTY_CASES}{count}",
            "missing or incorrect property case count")


def verify_fault_tests(transcript, profile):
    harnesses = profile["harnesses"]
    require(all(count == 1 for count in harnesses.values()), "fault transcript requires each exact test once")
    verify_rust_tests(transcript, profile)


def verify_interleaved_differential(transcript, profile):
    require(profile["harnesses"] == {"interleaved-forks": 1}, "unknown differential oracle")
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

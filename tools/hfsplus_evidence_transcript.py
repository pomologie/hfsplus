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
PROPERTY_RESULT = re.compile(r"test (\S+) \.\.\. (ok|FAILED|ignored)")
PROPERTY_SUMMARY = re.compile(
    r"test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; \d+ filtered out; finished in \d+\.\d+s"
)
PROPERTY_CASES = "HFS+ property cases checked: "


def verify_kani(transcript, harnesses, versions):
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


def verify_property_test(transcript, harnesses, versions):
    require(len(harnesses) == 1, "property transcript requires one exact test")
    (name, count), = harnesses.items()
    lines = transcript.splitlines()
    for tool in ("cargo", "rustc"):
        observed = [line for line in lines if line.startswith(f"{tool} ")]
        pinned = rf"{tool} {re.escape(versions[tool])}(?: \([^()]+\))?"
        require(len(observed) == 1 and re.fullmatch(pinned, observed[0]),
                f"transcript has the wrong {tool} version")
    starts = [index for index, line in enumerate(lines) if re.fullmatch(r"running \d+ tests?", line)]
    results = [(index, match.groups()) for index, line in enumerate(lines)
               if (match := PROPERTY_RESULT.fullmatch(line))]
    summaries = [index for index, line in enumerate(lines) if line.startswith("test result: ")]
    require(len(starts) == 1 and lines[starts[0]] == "running 1 test"
            and len(results) == 1 and results[0][1] == (name, "ok")
            and len(summaries) == 1 and PROPERTY_SUMMARY.fullmatch(lines[summaries[0]]),
            "missing, unexpected, or unsuccessful exact property test")
    headers = [index for index, line in enumerate(lines) if line == f"---- {name} stdout ----"]
    cases = [index for index, line in enumerate(lines) if line.startswith(PROPERTY_CASES)]
    require(len(headers) == 1 and len(cases) == 1 and cases[0] == headers[0] + 1
            and starts[0] < results[0][0] < headers[0] < cases[0] < summaries[0]
            and lines[cases[0]] == f"{PROPERTY_CASES}{count}",
            "missing or incorrect property case count")

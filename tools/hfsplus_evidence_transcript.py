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

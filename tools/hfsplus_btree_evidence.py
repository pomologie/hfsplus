"""Verify the H-V02 native B-tree evidence."""

from collections import Counter
from hashlib import sha256

from hfsplus_evidence_transcript import require, verify_tool_version
from hfsplus_fork_evidence import NATIVE_TEST, verify_exact_test, verify_fork_native


PACKED_LIVE_TEST = "packed_live::fresh_native_leaf_chains_match_packed_records"


def verify_packed_wire_differential(transcript, profile):
    bounds = profile["bounds"]
    expected = Counter({
        f"HFS+ {kind} {tree}": count
        for kind, counts in (("node", bounds["nodes"]), ("record", bounds["records"]))
        for tree, count in counts.items()
    })
    count = expected.total()
    lines = transcript.splitlines()
    require(len(lines) == count + 5 and profile["harnesses"] == {"retained-btree-wire": count},
            "incomplete retained B-tree comparison")
    for tool in ("Python", "cargo", "rustc"):
        verify_tool_version(lines[:3], tool, profile["tool_versions"][tool.lower()])
    require(lines[3] == f"Verified packed fixture ledgers SHA-256 {bounds['fixture_sha256']}",
            "incorrect retained B-tree fixture ledger digest")

    rows = lines[4:-1]
    digest = sha256("\n".join(rows).encode()).hexdigest()
    require(Counter(row.partition("|")[0] for row in rows) == expected
            and digest == bounds["wire_rows_sha256"]
            and lines[-1] == f"Compared {count} packed native wire rows SHA-256 {digest}",
            "incorrect retained B-tree descriptor or record rows")


def verify_packed_live_native(transcript, profile):
    require(profile["harnesses"] == {NATIVE_TEST: 1, PACKED_LIVE_TEST: 1},
            "wrong H-V02 native harnesses")
    lines = transcript.splitlines()
    starts = [index for index, line in enumerate(lines) if line == "running 1 test"]
    require(len(starts) == 2, "missing or duplicate native packed leaf replay")
    verify_fork_native("\n".join(lines[:starts[1]]), profile | {"harnesses": {NATIVE_TEST: 1}})

    leaf_reports = [
        f"Native packed {tree} leaf nodes {counts['leaves']} records {counts['records']} "
        f"ordered pairs {counts['ordered_pairs']}"
        for tree, counts in profile["bounds"]["packed_trees"].items()
    ]
    report = ["successes:", f"---- {PACKED_LIVE_TEST} stdout ----", *leaf_reports,
              "successes:", f"    {PACKED_LIVE_TEST}"]
    verify_exact_test(lines[starts[1]:], lines[1:3], profile, PACKED_LIVE_TEST, report)

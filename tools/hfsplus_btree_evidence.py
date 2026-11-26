"""Verify the H-V02 retained B-tree wire comparison."""

from collections import Counter
from hashlib import sha256

from hfsplus_evidence_transcript import require, verify_tool_version


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

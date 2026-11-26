"""Regression coverage for the H-V03 transcript contract."""

import json
from pathlib import Path
import unittest

from hfsplus_evidence_transcript import EvidenceError
from hfsplus_name_evidence import NAME_VALIDATORS, CORPUS, SEEDED_NAMES, SEPARATORS


ROOT = Path(__file__).resolve().parents[1]
PROFILES = json.loads((ROOT / "verification/release-profile.json").read_text(encoding="utf-8"))["profiles"]
RUST = ["cargo 1.99.1", "rustc 1.99.1"]
HOST = [*RUST, "Python 3.14.0", "macOS 14.7.2 build 23H311 arch arm64"]


def format_run(name, report):
    lines = [f"HFS+ name test: {name}", "running 1 test", f"test {name} ... ok", "", "successes:"]
    if report:
        lines += ["", f"---- {name} stdout ----", *report, "", "", "successes:"]
    return [*lines, f"    {name}", "", "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 15 filtered out; finished in 0.01s", ""]


class NameEvidenceTests(unittest.TestCase):
    def verify(self, method, prefix, reports, changes):
        profile = PROFILES[f"names-{method}"]
        verifier = NAME_VALIDATORS[method]
        lines = list(prefix)
        for name in profile["harnesses"]:
            lines.extend(format_run(name, reports.get(name)))
        good = "\n".join(lines)
        verifier(good, profile)
        first = next(iter(profile["harnesses"]))
        changed_invocations = [
            ("running 1 test", "running 0 tests"),
            (f"HFS+ name test: {first}", "HFS+ name test: unrelated_case"),
            (f"test {first} ... ok", f"test {first} ... ignored"),
        ]
        for old, replacement in [*changes, *changed_invocations]:
            with self.subTest(method=method, old=old):
                self.assertIn(old, good)
                with self.assertRaises(EvidenceError):
                    verifier(good.replace(old, replacement, 1), profile)
        with self.assertRaises(EvidenceError):
            verifier(good + "\nextra result", profile)

    def test_property_requires_each_seed_and_scope(self):
        reports = {
            CORPUS: [
                "Kernel / CaseFolded passed 830584 triples over 94 stored names",
                "Kernel / Binary passed 830584 triples over 94 stored names",
                "Unicode32Scalars / CaseFolded passed 778688 triples over 92 stored names",
                "Unicode32Scalars / Binary passed 778688 triples over 92 stored names",
            ],
            SEEDED_NAMES: ["HFS+ generated name triples: 4096 at seed 0x4846532b, up to 64 scalars and 255 stored units"],
            SEPARATORS: ["verified 16 equal separators over 198 leaf records",
                         "HFS+ generated separator lookups: 1024 at seed 0x53455041"],
        }
        self.verify("property", RUST, reports, [
            ("4096 at seed", "4095 at seed"),
            ("0x53455041", "0x53455040"),
            ("830584 triples", "830583 triples"),
            ("64 scalars", "63 scalars"),
            (reports[CORPUS][0], "\n".join((reports[CORPUS][0], reports[CORPUS][0]))),
        ])

    def test_differential_requires_live_scopes_fixture_and_georgian_witness(self):
        bounds = PROFILES["names-differential"]["bounds"]
        self.verify("differential", [
            *HOST,
            f"Unicode 3.2 table SHA-256: {bounds['unicode_table_sha256']}",
            "CoreServices HFS+ model: 65536 singleton units, 329 overrides, 16 ignorables",
            "CoreServices HFS+ samples: 40000 seeded pairs, 329 embedded overrides, 76 directed pairs, at most 255 units, seed 0x4846532b",
            f"CoreServices HFS+ rules SHA-256: {bounds['rules_sha256']}",
            f"CoreServices HFS+ fixture SHA-256: {bounds['fixture_sha256']}",
            "Modern Unicode 16.0.0 Georgian keys: 2d00 10d0",
        ], {}, [
            ("40000 seeded pairs", "0 seeded pairs"),
            ("2d00 10d0", "2d00 2d00"),
            (bounds["fixture_sha256"], "0" * 64),
            ("23H311", "different-build"),
        ])

    def test_mounted_evidence_requires_detach_counts_and_fixture(self):
        digest = PROFILES["names-native"]["bounds"]["fixture_sha256"]
        self.verify("native", [
            *HOST,
            "Mounted HFS+ image: 67108864 bytes, partitionless, detached",
            "Mounted HFS+ names: 63 normalization rows, 20 lookup pairs, 6 lengths, 25 ordered keys, 10 path observations, 3 mapped keys",
            f"Mounted HFS+ fixture SHA-256: {digest}",
        ], {}, [
            ("67108864 bytes", "33554432 bytes"),
            ("partitionless, detached", "partitionless, attached"),
            ("20 lookup pairs", "0 lookup pairs"),
            (digest, "0" * 64),
            ("arch arm64", "arch x86_64"),
        ])


if __name__ == "__main__":
    unittest.main()

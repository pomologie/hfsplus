"""Exercise HFS+ profile and release evidence checks in disposable repositories."""

from copy import deepcopy
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

from check_release_evidence import EvidenceError, POLICY_PATH, REGISTRY_PATH, archive_hash, validate


PROFILE = "format-kani"
HARNESS = "object::verification::sample"
TRANSCRIPT = f"""Kani Rust Verifier 0.68.0 (cargo plugin)
CBMC 6.11.0
Checking harness {HARNESS}...
 ** 0 of 5 failed (1 unreachable)
 ** 2 of 2 cover properties satisfied
VERIFICATION:- SUCCESSFUL
Manual Harness Summary:
Complete - 1 successfully verified harnesses, 0 failures, 1 total.
"""


class ReleaseEvidenceTests(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory(prefix="hfsplus-release-evidence-")
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name).resolve() / "candidate"
        self.root.mkdir()
        self.bundle = self.root.with_name("bundle.json")
        self.log = self.root.with_name("kani.log")
        profile = {
            "obligation": "H-V01", "method": "kani", "features": [],
            "command": ["bash", "crates/formats/hfsplus-format/verification/check.sh"],
            "tool_versions": {"kani": "0.68.0", "cbmc": "6.11.0"}, "bounds": {"scalar_bits": 64},
            "harnesses": {HARNESS: 2},
        }
        self.policy = {"schema_version": 1, "milestone": "0.1.0", "profiles": {PROFILE: profile}}
        self.registry = {"project": "hfsplus", "methods": {"kani": "proof", "native": "native"}, "obligations": [{
            "id": "H-V01", "activation": "0.1.0", "mandatory_when_activated": True,
            "required_methods": ["kani"],
        }]}
        self.git("init", "--quiet")
        self.commit_policy()
        self.record = {
            "schema_version": 1, "obligation_id": "H-V01", "profile": PROFILE, "method": "kani",
            "status": "pass", "commit": self.commit, "dirty": False, "source_hash": self.digest,
            "features": [], "tool_versions": profile["tool_versions"], "command": profile["command"],
            "assumptions": [], "bounds": profile["bounds"], "artifacts": [], "duration_ms": 1,
            "runner": {"os": "macos", "arch": "aarch64"}, "exit_code": 0, "proof": {"theorem": PROFILE},
        }
        self.set_transcript(TRANSCRIPT)

    def git(self, *args):
        return subprocess.check_output(["git", "-C", self.root, *args], text=True).strip()

    def commit_policy(self):
        (self.root / "verification").mkdir(exist_ok=True)
        (self.root / POLICY_PATH).write_text(json.dumps(self.policy), encoding="utf-8")
        (self.root / REGISTRY_PATH).write_text(json.dumps(self.registry), encoding="utf-8")
        self.git("add", "verification")
        self.git("-c", "user.name=HFS+ test", "-c", "user.email=hfsplus-test@example.invalid",
                 "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "fixture policy")
        self.commit = self.git("rev-parse", "HEAD")
        self.digest = archive_hash(self.root, self.commit)

    def set_transcript(self, value):
        self.log.write_text(value, encoding="utf-8")
        self.record["artifacts"] = [{
            "name": "transcript", "uri": self.log.name, "sha256": hashlib.sha256(value.encode()).hexdigest(),
        }]

    def check(self, records, requested_profile=None):
        self.bundle.write_text(json.dumps(records), encoding="utf-8")
        return validate(self.root, self.bundle, requested_profile)

    def test_accepts_matching_candidate_and_transcript(self):
        self.assertEqual(self.check([self.record]), (1, "0.1.0", self.commit))

    def test_archive_digest_ignores_worktree_line_ending_configuration(self):
        (self.root / ".gitattributes").write_text("*.txt text\n", encoding="utf-8")
        (self.root / "fixture.txt").write_bytes(b"first\nsecond\n")
        self.git("add", ".gitattributes", "fixture.txt")
        self.commit_policy()
        expected = self.digest
        self.git("config", "core.autocrlf", "true")
        self.git("config", "core.eol", "crlf")
        unpinned = subprocess.check_output(
            ["git", "-C", self.root, "-c", "tar.umask=0002", "archive", "--format=tar", self.commit]
        )
        self.assertNotEqual(hashlib.sha256(unpinned).hexdigest(), expected)
        self.assertEqual(archive_hash(self.root, self.commit), expected)

    def test_future_and_unscheduled_methods_do_not_activate_in_initial_release(self):
        for identifier, activation in (("H-V02", "0.8+"), ("H-V03", "later")):
            self.registry["obligations"].append({
                "id": identifier, "activation": activation, "mandatory_when_activated": True,
                "required_methods": ["native"],
            })
        self.commit_policy()
        self.record.update(commit=self.commit, source_hash=self.digest)
        self.assertEqual(self.check([self.record]), (1, "0.1.0", self.commit))

    def test_missing_duplicate_wrong_profile_and_incompatible_method(self):
        cases = [([], "missing evidence"), ([self.record, self.record], "duplicate evidence"),
                 ([self.record | {"profile": "other"}], "unexpected or duplicate evidence"),
                 ([self.record | {"method": "native"}], "does not match profile")]
        for records, expected in cases:
            with self.subTest(expected=expected), self.assertRaisesRegex(EvidenceError, expected):
                self.check(records)

    def test_release_mode_does_not_conflate_a_profile_pass_with_the_complete_matrix(self):
        self.registry["obligations"][0]["required_methods"].append("native")
        self.commit_policy()
        self.record.update(commit=self.commit, source_hash=self.digest)
        with self.assertRaisesRegex(EvidenceError, "release policy omits H-V01/native"):
            self.check([self.record])
        self.assertEqual(self.check([self.record], PROFILE), (1, "0.1.0", self.commit))

    def test_rejects_sibling_project_registry(self):
        self.registry["project"] = "apfs"
        self.commit_policy()
        with self.assertRaisesRegex(EvidenceError, "does not have an HFS\\+ obligation registry"):
            self.check([self.record], PROFILE)

    def test_stale_commit_archive_or_dirty_candidate(self):
        for changes in ({"commit": "0" * 40}, {"source_hash": "0" * 64}):
            with self.subTest(changes=changes), self.assertRaisesRegex(EvidenceError, "stale evidence"):
                self.check([self.record | changes])
        for path in (self.root / "untracked", self.root / POLICY_PATH):
            with self.subTest(path=path):
                path.write_text("changed", encoding="utf-8")
                with self.assertRaisesRegex(EvidenceError, "checkout has tracked or untracked changes"):
                    self.check([self.record])
                if path == self.root / POLICY_PATH:
                    self.git("checkout", "--", POLICY_PATH)
                else:
                    path.unlink()

    def test_invocation_status_bounds_features_and_tools(self):
        cases = [({"status": "timeout"}, "clean successful"), ({"status": "unsupported"}, "clean successful"),
                 ({"dirty": True}, "clean successful"), ({"exit_code": False}, "clean successful"),
                 ({"bounds": {"scalar_bits": 32}}, "bounds"), ({"features": ["std"]}, "features"),
                 ({"command": ["bash", "other.sh"]}, "does not match profile"),
                 ({"tool_versions": {"kani": "0.68.0", "cbmc": "6.10.0"}}, "tool versions")]
        for changes, expected in cases:
            with self.subTest(changes=changes), self.assertRaisesRegex(EvidenceError, expected):
                self.check([self.record | changes])

    def test_transcript_digest_and_path_are_checked(self):
        self.log.write_text(TRANSCRIPT + "edited\n", encoding="utf-8")
        with self.assertRaisesRegex(EvidenceError, "artifact SHA-256 mismatch"):
            self.check([self.record])
        original = deepcopy(self.record)
        for uri in ("../kani.log", "/tmp/kani.log"):
            with self.subTest(uri=uri), self.assertRaisesRegex(EvidenceError, "artifact path"):
                self.record["artifacts"][0]["uri"] = uri
                self.check([self.record])
        self.record = original
        self.bundle = self.root.with_name("reports") / "bundle.json"
        self.bundle.parent.mkdir()
        (self.bundle.parent / self.log.name).symlink_to(self.log)
        with self.assertRaisesRegex(EvidenceError, "artifact escapes"):
            self.check([self.record])

    def test_vacuous_incomplete_or_off_scope_transcripts_are_rejected(self):
        cases = [("0 of 5 failed (1 unreachable)", "0 of 0 failed", "vacuous"),
                 ("1 unreachable", "5 unreachable", "vacuous"),
                 ("2 of 2 cover properties satisfied", "1 of 2 cover properties satisfied", "unreached"),
                 ("2 of 2 cover properties satisfied", "", "missing"),
                 ("CBMC 6.11.0", "CBMC 6.10.0", "wrong CBMC version"),
                 ("CBMC 6.11.0", "CBMC 6.11.0\nCBMC 6.10.0", "wrong CBMC version"),
                 (f"Checking harness {HARNESS}", "Checking harness other", "unexpected Kani harness"),
                 ("0 failures, 1 total.", "1 failures, 1 total.", "incomplete Kani summary")]
        for old, replacement, expected in cases:
            self.set_transcript(TRANSCRIPT.replace(old, replacement))
            with self.subTest(replacement=replacement), self.assertRaisesRegex(EvidenceError, expected):
                self.check([self.record])


if __name__ == "__main__":
    unittest.main()

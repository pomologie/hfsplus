"""Exercise HFS+ profile and release evidence checks in disposable repositories."""

from copy import deepcopy
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

from check_release_evidence import EvidenceError, POLICY_PATH, REGISTRY_PATH, archive_hash, validate
from hfsplus_evidence_transcript import (
    verify_differential, verify_interleaved_native, verify_native, verify_volume_wire_differential,
)


PROFILE = "format-kani"
HARNESS = "object::verification::sample"
PROPERTY_TEST = "positioned_reads_tests::model"
PROPERTY_CASES = 3840
TRANSCRIPT = f"""Kani Rust Verifier 0.68.0 (cargo plugin)
CBMC 6.11.0
Checking harness {HARNESS}...
 ** 0 of 5 failed (1 unreachable)
 ** 2 of 2 cover properties satisfied
VERIFICATION:- SUCCESSFUL
Manual Harness Summary:
Complete - 1 successfully verified harnesses, 0 failures, 1 total.
"""
PROPERTY_TRANSCRIPT = f"""cargo 1.99.1
rustc 1.99.1
running 1 test
test {PROPERTY_TEST} ... ok

successes:

---- {PROPERTY_TEST} stdout ----
HFS+ property cases checked: {PROPERTY_CASES}

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.08s
"""
FAULT_TRANSCRIPT = """cargo 1.99.1
rustc 1.99.1
running 2 tests
test retry ... ok
test source_failure ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 3 filtered out; finished in 0.01s
"""


def interleaved_fixture():
    digest = hashlib.sha256(b"fixture").hexdigest()
    profile = {
        "harnesses": {"interleaved-forks": 1},
        "tool_versions": {"cargo": "1.99.1", "rustc": "1.99.1", "python": "3.14.0"},
        "bounds": {"image_bytes": 8388608, "image_sha256": digest, "catalog_records": 134,
                   "catalog_depth": 2, "continuation_keys": [[0, 16], [0, 32]],
                   "fork_lengths_bytes": [159761, 159775]},
    }
    transcript = f"""Python 3.14.0
cargo 1.99.1
rustc 1.99.1
Interleaved image 8388608 bytes SHA-256 {digest}
Independent raw reader found 134 unique catalog records at depth 2
Independent raw reader matched all 2 continuation keys [(0, 16), (0, 32)] and both complete forks (159761, 159775 bytes)
"""
    return profile, transcript


class ReleaseEvidenceTests(unittest.TestCase):
    def test_volume_wire_differential_requires_pinned_rows_and_ledger(self):
        rows = "HFS+ header volume HfsPlus 4096 2 0 0 8192 7168\nHFS+ fork volume catalog 4096 4096 1 1:1"
        digest = hashlib.sha256(rows.encode()).hexdigest()
        ledger = "a" * 64
        profile = {
            "bounds": {"native_headers": 1, "fork_rows": 1, "fixture_sha256": ledger, "wire_rows_sha256": digest},
            "harnesses": {"native-volume-wire": 2},
            "tool_versions": {"python": "3.14.0", "cargo": "1.99.1", "rustc": "1.99.1"},
        }
        transcript = (f"Python 3.14.0\ncargo 1.99.1\nrustc 1.99.1\n"
                      f"Verified native fixture ledgers SHA-256 {ledger}\n{rows}\n"
                      f"Compared 2 native wire rows SHA-256 {digest}\n")
        verify_volume_wire_differential(transcript, profile)
        for changed in (transcript.replace("1:1", "2:1"), transcript.replace(ledger, "b" * 64),
                        transcript.replace("Compared 2", "Compared 1")):
            with self.assertRaises(EvidenceError):
                verify_volume_wire_differential(changed, profile)

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

    def set_rust_profile(self, name, method, harnesses):
        profile = self.policy["profiles"].pop(PROFILE)
        profile.update(
            method=method, command=["bash", "check-rust-tests.sh"],
            tool_versions={"cargo": "1.99.1", "rustc": "1.99.1"},
            harnesses=harnesses,
        )
        self.policy["profiles"][name] = profile
        self.registry["methods"][method] = f"{method} checks"
        self.registry["obligations"][0]["required_methods"] = [method]
        self.commit_policy()
        self.record.update(
            commit=self.commit, source_hash=self.digest, profile=name,
            method=method, command=profile["command"], tool_versions=profile["tool_versions"],
        )
        self.record.pop("proof")

    def test_accepts_matching_candidate_and_transcript(self):
        self.assertEqual(self.check([self.record]), (1, "0.1.0", self.commit))

    def test_property_requires_the_exact_successful_test_and_case_count(self):
        property_profile = "sealed-positioned-property"
        self.set_rust_profile(property_profile, "property", {PROPERTY_TEST: PROPERTY_CASES})
        self.set_transcript(PROPERTY_TRANSCRIPT)
        self.assertEqual(self.check([self.record], property_profile), (1, "0.1.0", self.commit))

        for old, new, message in (
            ("rustc 1.99.1", "rustc 1.99.0", "wrong rustc version"),
            (f"test {PROPERTY_TEST} ... ok", "test other ... ok", "exact property test"),
            ("running 1 test", "running 0 tests", "exact property test"),
            ("1 passed; 0 failed", "0 passed; 1 failed", "exact property test"),
            (f"checked: {PROPERTY_CASES}", "checked: 3839", "property case count"),
            (f"HFS+ property cases checked: {PROPERTY_CASES}\n", "", "property case count"),
        ):
            with self.subTest(replacement=new):
                self.set_transcript(PROPERTY_TRANSCRIPT.replace(old, new, 1))
                with self.assertRaisesRegex(EvidenceError, message):
                    self.check([self.record], property_profile)

    def test_state_machine_requires_the_exact_draft_history_counts(self):
        name = "draft-identity-state-machine"
        test = "preserves_reference_ids_across_generated_binding_states_and_seal"
        self.policy["profiles"][PROFILE]["bounds"] = self.record["bounds"] = {
            "unbound_prefixes": 54, "sealed_histories": 18, "catalog_objects": 16,
        }
        self.set_rust_profile(name, "state-machine", {test: 72})
        counts = "HFS+ draft histories checked: 72 total, 54 pending, 18 sealed (16 catalog objects)"
        transcript = PROPERTY_TRANSCRIPT.replace(PROPERTY_TEST, test).replace(
            f"HFS+ property cases checked: {PROPERTY_CASES}", counts,
        )
        self.set_transcript(transcript)
        self.assertEqual(self.check([self.record], name), (1, "0.1.0", self.commit))

        for replacement in ("", counts.replace("54 pending", "53 pending"),
                            counts.replace("18 sealed", "19 sealed"), counts.replace("16 catalog", "15 catalog")):
            with self.subTest(replacement=replacement):
                self.set_transcript(transcript.replace(counts, replacement))
                with self.assertRaisesRegex(EvidenceError, "draft history counts"):
                    self.check([self.record], name)

    def test_fault_requires_each_exact_successful_test(self):
        fault_profile = "sealed-stream-faults"
        self.set_rust_profile(fault_profile, "fault", {"retry": 1, "source_failure": 1})
        self.set_transcript(FAULT_TRANSCRIPT)
        self.assertEqual(self.check([self.record], fault_profile), (1, "0.1.0", self.commit))
        expected = "test source_failure ... ok"
        for changed in ("", "test other ... ok", "test retry ... ok", "test source_failure ... ignored"):
            with self.subTest(changed=changed):
                self.set_transcript(FAULT_TRANSCRIPT.replace(expected, changed))
                with self.assertRaisesRegex(EvidenceError, "exact fault test"):
                    self.check([self.record], fault_profile)

    def test_differential_requires_the_exact_fixture_and_both_fork_results(self):
        profile, transcript = interleaved_fixture()
        digest = profile["bounds"]["image_sha256"]
        verify_differential(transcript, profile)
        for old, new in ((digest, "0" * len(digest)), ("134 unique", "133 unique"),
                         ("(0, 32)", "(0, 31)"), ("159775 bytes", "159774 bytes")):
            with self.subTest(replacement=new):
                with self.assertRaisesRegex(EvidenceError, "incorrect interleaved fixture"):
                    verify_differential(transcript.replace(old, new), profile)
        with self.assertRaisesRegex(EvidenceError, "incorrect interleaved fixture"):
            verify_differential(transcript.rsplit("\n", 2)[0], profile)

    def test_allocation_differential_requires_block_ownership_and_both_headers(self):
        profile, transcript = interleaved_fixture()
        profile["harnesses"] = {"interleaved-allocation": 1}
        profile["bounds"].update(total_blocks=2048, reserved_blocks=2, metadata_blocks=11,
                                 file_blocks=80, free_blocks=1955)
        comparison = ("Independent allocation matched both headers and bitmap across 2048 blocks: "
                      "2 reserved, 11 metadata, 80 file, 1955 free")
        transcript += comparison + "\n"
        verify_differential(transcript, profile)
        for changed in ("", comparison.replace("both headers", "one header"),
                        comparison.replace("11 metadata", "12 metadata"),
                        comparison.replace("1955 free", "1954 free")):
            with self.subTest(changed=changed):
                with self.assertRaisesRegex(EvidenceError, "incorrect independent allocation"):
                    verify_differential(transcript.replace(comparison, changed), profile)

    def test_native_requires_no_mount_no_write_and_matching_device_and_digest(self):
        profile, transcript = interleaved_fixture()
        profile["tool_versions"]["fsck_hfs"] = "650.140.2"
        attach = ["/usr/bin/hdiutil", "attach", "-readonly", "-nomount", "<fixture>"]
        fsck = ["/sbin/fsck_hfs", "-fn"]
        profile["bounds"].update(attachment_argv=attach, fsck_argv=fsck, volume_label="Interleaved Forks")
        digest = profile["bounds"]["image_sha256"]
        transcript += f"""Native attachment argv {json.dumps(attach)}
Native attachment checked unmounted /dev/disk42
Native fsck argv {json.dumps([*fsck, '/dev/disk42'])}
Can't open /dev/rdisk42: Permission denied
** /dev/rdisk42 (NO WRITE)
   Executing fsck_hfs (version hfs-650.140.2).
** Checking non-journaled HFS Plus Volume.
   The volume name is Interleaved Forks
** Checking extents overflow file.
** Checking catalog file.
** Checking multi-linked files.
** Checking catalog hierarchy.
** Checking volume bitmap.
** Checking volume information.
** The volume Interleaved Forks appears to be OK.
Native attachment detached /dev/disk42
Native no-write image SHA-256 {digest}
"""
        verify_interleaved_native(transcript, profile)
        for old, new in (
            ("-nomount", "-mount"), ("checked unmounted", "checked mounted"),
            ("-fn", "-fy"), ("(NO WRITE)", "(WRITE)"), ("hfs-650.140.2", "hfs-650.140.1"),
            ("appears to be OK", "repaired"), ("detached /dev/disk42", "detached /dev/disk43"),
            (f"Native no-write image SHA-256 {digest}", "Native no-write image SHA-256 invalid"),
        ):
            with self.subTest(replacement=new):
                with self.assertRaises(EvidenceError):
                    verify_interleaved_native(transcript.replace(old, new), profile)

    def test_native_allocation_requires_independent_counts_and_identical_digest(self):
        digest = hashlib.sha256(b"canonical").hexdigest()
        profile = {
            "harnesses": {"canonical-allocation-counts": 1},
            "tool_versions": {"cargo": "1.99.1", "rustc": "1.99.1", "python": "3.14.0",
                              "macos": "14.7.2", "macos_build": "23H311"},
            "bounds": {"image_sha256": digest, "total_blocks": 2048, "free_blocks": 2033,
                       "allocation_block_bytes": 4096},
        }
        transcript = f"""Python 3.14.0
cargo 1.99.1
rustc 1.99.1
macOS 14.7.2 build 23H311
canonical.hfs {digest}
macOS matched 2048 total and 2033 free 4096-byte allocation blocks
Native read-only allocation attachment detached
canonical.hfs {digest}
"""
        verify_native(transcript, profile)
        for old, new in (("23H311", "23H312"), ("2048 total", "2049 total"), ("2033 free", "2032 free"),
                         ("attachment detached", "attachment mounted"), (digest, "0" * len(digest))):
            with self.subTest(replacement=new):
                with self.assertRaisesRegex(EvidenceError, "incorrect native allocation"):
                    verify_native(transcript.replace(old, new, 1), profile)

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

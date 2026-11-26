# HFS+ release evidence

Run the checker with Git and Python 3.11 or later from a clean candidate worktree. Keep the evidence bundle and transcripts outside that worktree.

```sh
python3 -B tools/check_release_evidence.py --root . --bundle "$evidence_dir/bundle.json"
```

The release command derives the mandatory methods for the milestone from the committed [obligation registry](obligations.json). It requires a configured invocation for every active obligation and method, and a passing evidence record for every configured invocation. The current [release policy](release-profile.json) includes the `format-kani` geometry profile for H-V01 and the `btree-offset-kani` table profile for H-V02. The release command reports the remaining unconfigured methods and rejects the bundle until the matrix is complete.

Use `--profile format-kani` to check the geometry proofs separately. Run the policy's proof command from an isolated checkout of the candidate with Kani 0.68.0 and CBMC 6.11.0. Capture both output streams together. The [geometry verification guide](../crates/formats/hfsplus-format/verification/README.md) explains the proof scope and bounds.

```sh
bash crates/formats/hfsplus-format/verification/check.sh >"$evidence_dir/kani.log" 2>&1
git -c tar.umask=0002 -c core.autocrlf=false -c core.eol=lf archive --format=tar HEAD | shasum -a 256
python3 -B tools/check_release_evidence.py --root . --profile format-kani --bundle "$evidence_dir/bundle.json"
```

Use `--profile btree-offset-kani` with a record of `bash crates/formats/hfsplus-format/verification/check-btree-offsets.sh` to check the offset-table proofs independently. The [format verification guide](../crates/formats/hfsplus-format/verification/README.md#btree-offset-tables) describes the assumptions and scope.

The bundle is a JSON array of [schema v1 records](evidence.schema.json). Set `commit` to the exact Git commit and `source_hash` to the pinned archive SHA-256 above. Record the policy's feature set, command and bounds, the observed tool versions, the runner, elapsed time, and invocation exit code. Use the profile name for `proof.theorem` and include the invocation log as an artifact named `transcript`, with its SHA-256 and a path relative to the bundle.

The checker binds the record to the committed source and policy, verifies the artifact bytes, and checks successful Kani results and reached cover counts. The release process provides transcripts from real invocations. This profile and the standalone checker tests provide partial H-VER-02-T01 evidence. The HFS+ checker is adapted from Pomologie APFS commit `62877b6` and runs entirely within this repository. [Checker tests](../tools/test_check_release_evidence.py)

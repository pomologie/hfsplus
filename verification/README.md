# HFS+ release evidence

Run the checker with Git and Python 3.11 or later from a clean candidate worktree. Keep the evidence bundle and transcripts outside that worktree.

```sh
python3 -B tools/check_release_evidence.py --root . --bundle "$evidence_dir/bundle.json"
```

The release command derives the mandatory methods for the milestone from the committed [obligation registry](obligations.json). It requires a configured invocation for every active obligation and method, and a passing evidence record for every configured invocation. The current [release policy](release-profile.json) includes `format-kani` for H-V01 geometry, `btree-offset-kani` for H-V02 offset tables, H-V06 profiles for allocation proofs, block-set properties and macOS counts, and five H-V07 profiles for range admission, bounded positioned reads, synthetic stream faults, independent image comparison, and native no-write checking. The release command reports the remaining unconfigured methods and rejects the bundle until the matrix is complete.

Use `--profile format-kani` to check the geometry proofs separately. Run the policy's proof command from an isolated checkout of the candidate with Kani 0.68.0 and CBMC 6.11.0. Capture both output streams together. The [geometry verification guide](../crates/formats/hfsplus-format/verification/README.md) explains the proof scope and bounds.

```sh
bash crates/formats/hfsplus-format/verification/check.sh >"$evidence_dir/kani.log" 2>&1
git -c tar.umask=0002 -c core.autocrlf=false -c core.eol=lf archive --format=tar HEAD | shasum -a 256
python3 -B tools/check_release_evidence.py --root . --profile format-kani --bundle "$evidence_dir/bundle.json"
```

Use `--profile btree-offset-kani` with a record of `bash crates/formats/hfsplus-format/verification/check-btree-offsets.sh` to check the offset-table proofs independently. The [format verification guide](../crates/formats/hfsplus-format/verification/README.md#btree-offset-tables) describes the assumptions and scope.

Use `--profile fresh-allocation-kani` with a record of `bash crates/filesystem/hfsplus-builder/verification/check-allocation.sh` to check the two bitmap and two reservation transition proofs independently. Each harness must reach both covers. [Builder verification guide](../crates/filesystem/hfsplus-builder/verification/README.md)

Use `--profile fresh-allocation-property` with a record of `bash crates/filesystem/hfsplus-builder/verification/check-allocation-set.sh`. The exact positive test compares every stored allocation bit and count with the reference block set for four valid layouts.

Use `--profile fresh-allocation-native` with a record of `env -u TMPDIR python3 -B crates/filesystem/hfsplus-builder/verification/check-canonical-volume.py --allocation-counts-only`. Set `fixture_hash` to the canonical digest. This scoped Apple `statvfs` comparison covers one fresh synthetic image through a read-only software mount. It compares total and free bytes with the serialized header and requires an unchanged digest after detach. H-V06 still needs differential release evidence.

Use `--profile sealed-placement-kani` with a record of `bash crates/filesystem/hfsplus-builder/verification/check-placement.sh` to check the admission proof independently. The [builder verification guide](../crates/filesystem/hfsplus-builder/verification/README.md) records the positioned-read properties and the separate hosted and native checks.

Use `--profile sealed-positioned-property` with a record of `bash crates/filesystem/hfsplus-builder/verification/check-positioned-reads.sh` to check the exact positioned-read property test on Rust 1.99.1. The test enumerates 3,840 layouts with five placements and lengths of one or two bytes.

Use `--profile sealed-stream-faults` with a record of `bash crates/filesystem/hfsplus-builder/verification/check-sealed-faults.sh` to check the four exact in-memory fault tests on Rust 1.99.1. They cover source failures including premature EOF, detected source identity change, a sink error, cancellation, and retry.

Use `--profile sealed-image-differential` with a record of `python3 -B crates/filesystem/hfsplus-builder/verification/check-canonical-volume.py --interleaved-forks`. The independent Python reader checks one deterministic 8 MiB image, 134 catalog records, four continuation keys, and both complete fork payloads with zero block padding. Use Python 3.14.0 and Rust 1.99.1 for this release profile and set `fixture_hash` to the pinned image digest. The comparator reads an ordinary fixture file.

Use `--profile sealed-image-native` with a record of `env -u TMPDIR python3 -B crates/filesystem/hfsplus-builder/verification/check-canonical-volume.py --interleaved-fsck-only`. Set `fixture_hash` to the same pinned image digest. This macOS profile checks one clean, nonjournaled, generated volume with Apple `fsck_hfs` 650.140.2 on a temporary read-only software device. The script verifies the device is unmounted, requires successful `-fn`, detaches it, and checks that the entire image digest stayed identical. The scope is filesystem consistency for that fixture. [Native verifier](../crates/filesystem/hfsplus-builder/verification/readonly_fsck_oracle.py)

The bundle is a JSON array of [schema v1 records](evidence.schema.json). Set `commit` to the exact Git commit and `source_hash` to the pinned archive SHA-256 above. Record the policy's feature set, command and bounds, the observed tool versions, the runner, elapsed time, and invocation exit code. For Kani records, use the profile name for `proof.theorem`. Include the invocation log as an artifact named `transcript`, with its SHA-256 and a path relative to the bundle.

The checker binds the record to the committed source and policy and verifies the artifact bytes. It checks the exact output each profile requires, including Kani covers, completed Rust test cases, or the differential fixture digest and readback. The release process provides transcripts from real invocations. These profiles and the standalone checker tests provide partial H-VER-02-T01 evidence. The HFS+ checker is adapted from Pomologie APFS commit `62877b6` and runs entirely within this repository. [Checker tests](../tools/test_check_release_evidence.py)

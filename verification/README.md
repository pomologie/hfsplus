# HFS+ release evidence

Run the checker with Git and Python 3.11 or later from a clean candidate worktree. Keep the evidence bundle and transcripts outside that worktree.

```sh
python3 -B tools/check_release_evidence.py --root . --bundle "$evidence_dir/bundle.json"
```

The release command derives the mandatory methods for the milestone from the committed [obligation registry](obligations.json). It requires a configured invocation for every active obligation and method, and a passing evidence record for every configured invocation. The current [release policy](release-profile.json) includes `format-kani` for H-V01 geometry, `btree-offset-kani` for H-V02 offset tables, three H-V03 name profiles, the H-V05 stable-ID property, draft state machine, independent wire comparison, and native mount, H-V06 profiles for allocation proofs, block-set properties, independent raw allocation comparison, and macOS counts, and five H-V07 profiles for range admission, bounded positioned reads, synthetic stream faults, independent image comparison, and native no-write checking. The release command reports the remaining unconfigured methods and rejects the bundle until the matrix is complete.

Use `--profile format-kani` to check the geometry proofs separately. Run the policy's proof command from an isolated checkout of the candidate with Kani 0.68.0 and CBMC 6.11.0. Capture both output streams together. The [geometry verification guide](../crates/formats/hfsplus-format/verification/README.md) explains the proof scope and bounds.

```sh
bash crates/formats/hfsplus-format/verification/check.sh >"$evidence_dir/kani.log" 2>&1
git -c tar.umask=0002 -c core.autocrlf=false -c core.eol=lf archive --format=tar HEAD | shasum -a 256
python3 -B tools/check_release_evidence.py --root . --profile format-kani --bundle "$evidence_dir/bundle.json"
```

Use `--profile btree-offset-kani` with a record of `bash crates/formats/hfsplus-format/verification/check-btree-offsets.sh` to check the offset-table proofs independently. The [format verification guide](../crates/formats/hfsplus-format/verification/README.md#btree-offset-tables) describes the assumptions and scope.

Use `--profile btree-packed-native-property` with a record of `python3 -B crates/filesystem/hfsplus-reader/verification/check-packed-nodes.py`. The test compares 228 borrowed record ranges with the on-disk offset words in 21 retained leaf and index nodes. It decodes catalog, extent and attribute keys and compares all 3,517 within-node key pairs against the stored order.

### Historical name release evidence

The H-V03 `names-property` profile binds exhaustive ordered triples under both normalization policies and both comparators, 4,096 seeded Unicode triples, planner ordering, and 16 exact index separators with 1,024 seeded lookups. The `names-differential` profile binds the live CoreServices HFS Extended oracle, the pinned Unicode tables, the optical name fixtures and the historical Georgian collision. The `names-native` profile binds the mounted 64 MiB HFS+ capture, successful detach, raw catalog key order, and the separate Rust fixture replay. Use the CoreServices pair TSV hash for the differential record's `fixture_hash` and the mounted name TSV hash for the native record's `fixture_hash`. [Name evidence and fixture provenance](../crates/foundation/hfsplus-pomologie-core/UNICODE32.md)

```sh
python3 -B crates/foundation/hfsplus-pomologie-core/verification/check-name-evidence.py property
python3 -B crates/foundation/hfsplus-pomologie-core/verification/check-name-evidence.py differential
env -u TMPDIR python3 -B crates/foundation/hfsplus-pomologie-core/verification/check-name-evidence.py native
```

All three profiles passed the release checker at clean revision `84a365fff0eba8b353f824aebb9022fc48cd4179` on arm64 macOS 14.7.2 (23H311). The Git archive SHA-256 was `9e850e86c182eca023fd32733a24b8e4ec9f3a4249e6160637e606b84eb827bc`. The local evidence directory is `target/verification-evidence/v0.1.0/84a365f-names/` and its `bundle.json` SHA-256 is `761c5bc01421ca1dc170ae77090237236bfccc4e28b9a9d2b06a1f7385ddc9bd`. For an individual check, set `--bundle` to `property.json`, `differential.json`, or `native.json` in that directory and select the corresponding `--profile`.

| Profile | Observed scope |
| --- | --- |
| `names-property` | 3,218,544 ordered comparisons across the historical corpora, 4,096 seeded triples, stable planner IDs and catalog order, 198 leaf records, and 1,024 seeded lookups. |
| `names-differential` | 65,536 CoreServices singleton weights, 329 override checks, 16 ignorables, 40,000 seeded pairs, 76 directed pairs, and the historical Georgian collision distinguished from Unicode 16.0.0 folding. |
| `names-native` | 63 normalization observations, 20 lookup pairs, six name-length cases, 25 ordered catalog keys, ten path observations, three mapped keys, and successful detach. |

### Fork and metadata release evidence

Use `--profile fork-windows-property` for every in-bounds positioned read of the stored data and resource forks through five aliases, across the retained Apple chained and chain-free fixtures. The exact test checks inode IDs and link counts for each alias and compares all 4,810 read windows against the known payload slices. The largest fork is 24 bytes. The enumeration includes zero-length windows and the empty resource fork.

Use `--profile forks-differential` for the standalone byte reader on the pinned interleaved image and the exact production-reader replays for the retained Apple fixtures. It checks four continuation keys, both complete interleaved payloads, three exact Rust tests, and the matching fixture digests. Set `fixture_hash` to the interleaved image digest.

Use `--profile forks-native` for a freshly generated 64 MiB Apple HFS+ tree. The command checks the device with no-write `fsck_hfs`, mounts read-only to capture native metadata, and detaches before Rust reads back the raw image. Rust verifies both 5 MiB forks across 20 extents through each of two hard-link names, plus directory listings, attributes, and a literal dangling symlink. Set `fixture_hash` to the stable tree manifest digest. The generated image's digest is recorded separately because its timestamps and ownership come from the native run. [Native metadata fixture provenance](../docs/features/FORMAT.md)

```sh
python3 -B crates/filesystem/hfsplus-reader/verification/check-fork-evidence.py property
python3 -B crates/filesystem/hfsplus-reader/verification/check-fork-evidence.py differential
env -u TMPDIR python3 -B crates/filesystem/hfsplus-reader/verification/check-fork-evidence.py native
```

The differential and native profiles passed the release checker at clean revision `b82a34fc34b27c40792b6f1cd303434b6d889304` on arm64 macOS 14.7.2 (23H311). The Git archive SHA-256 was `995bcf1d79afff2ea6a064439e18cf77060aea4267baf8d0bc1565908680f80a`. The local evidence directory is `target/verification-evidence/v0.1.0/b82a34f-forks/`. Its `bundle.json` SHA-256 is `298cb783b80d3eed052b0abbe20e3ab5009128bc05b3572d01cf6cf808fbffd6`. Select `differential.json` or `native.json` for an individual check.

The property profile passed at clean revision `269f9c149223c03cc56e0e7390d2443d75b5355d` with Rust 1.99.1 on arm64 macOS. The Git archive SHA-256 was `1adecd6b343b12ffb3e80829dfdc2ac5c4ef5bdf14fcd9d43dd0209006cf887c`. The local evidence directory is `target/verification-evidence/v0.1.0/269f9c1-fork-property/`, with bundle SHA-256 `831a880800d9d9cd877e43c44d891223ec74df91a0f5971e2a37784e321bc48b`. H-V04 still requires separate fuzz evidence.

### Allocation and sealed output

Use `--profile stable-id-property` with a record of `bash crates/filesystem/hfsplus-builder/verification/check-stable-id-property.sh` and Rust 1.99.1 on `PATH`. The test enumerates 192 parent-closed ASCII tree shapes of up to 13 catalog objects. It tests each with two manifest orders and two generated payload lengths, using a component-wise case-folded reference order. It checks draft catalog and inode IDs before and after binding, then verifies each sealed entry both by CNID thread and by parent/name lookup.

Use `--profile draft-identity-state-machine` with a record of `bash crates/filesystem/hfsplus-builder/verification/check-draft-identity.sh` and Rust 1.99.1 on `PATH`. The exact test compares draft and sealed identities against a 16-object reference catalog for all binding orders and payload-length rotations. The checker requires the actual counts of 54 pending and 18 sealed histories.

Use `--profile draft-identity-differential` with a record of `python3 -B crates/filesystem/hfsplus-builder/verification/check-frozen-identity.py`. Set `fixture_hash` to the pinned 8 MiB frozen-draft image digest. The independent wire reader verifies all 16 catalog objects and their threads, the next CNID, generated data and resource forks, and the two regular hard-link stubs and their shared inode.

Use `--profile draft-identity-native` with a record of `env -u TMPDIR python3 -B crates/filesystem/hfsplus-builder/verification/check-frozen-identity.py --native`. Set `fixture_hash` to the same pinned digest. A read-only Apple software mount checks root and generated CNIDs, generated data and resource forks, and both aliases' shared inode, link count, and data. The profile requires the recorded detach and unchanged digest.

Use `--profile fresh-allocation-kani` with a record of `bash crates/filesystem/hfsplus-builder/verification/check-allocation.sh` to check the two bitmap and two reservation transition proofs independently. Each harness must reach both covers. [Builder verification guide](../crates/filesystem/hfsplus-builder/verification/README.md)

Use `--profile fresh-allocation-property` with a record of `bash crates/filesystem/hfsplus-builder/verification/check-allocation-set.sh`. The exact positive test compares every stored allocation bit and count with the reference block set for four valid layouts.

Use `--profile fresh-allocation-differential` with a record of `python3 -B crates/filesystem/hfsplus-builder/verification/check-canonical-volume.py --allocation-differential`. Set `fixture_hash` to the pinned interleaved image digest. An independent wire reader reconstructs block ownership from reserved regions, metadata forks, and inline and overflow file extents. It compares the full allocation bitmap, both headers, and their free count for all 2,048 blocks of this fixture.

Use `--profile fresh-allocation-native` with a record of `env -u TMPDIR python3 -B crates/filesystem/hfsplus-builder/verification/check-canonical-volume.py --allocation-counts-only`. Set `fixture_hash` to the canonical digest. This scoped Apple `statvfs` comparison covers one fresh synthetic image through a read-only software mount. It compares total and free bytes with the serialized header and requires an unchanged digest after detach.

Use `--profile sealed-placement-kani` with a record of `bash crates/filesystem/hfsplus-builder/verification/check-placement.sh` to check the admission proof independently. The [builder verification guide](../crates/filesystem/hfsplus-builder/verification/README.md) records the positioned-read properties and the separate hosted and native checks.

Use `--profile sealed-positioned-property` with a record of `bash crates/filesystem/hfsplus-builder/verification/check-positioned-reads.sh` to check the exact positioned-read property test on Rust 1.99.1. The test enumerates 3,840 layouts with five placements and lengths of one or two bytes.

Use `--profile sealed-stream-faults` with a record of `bash crates/filesystem/hfsplus-builder/verification/check-sealed-faults.sh` to check the four exact in-memory fault tests on Rust 1.99.1. They cover source failures including premature EOF, detected source identity change, a sink error, cancellation, and retry.

Use `--profile sealed-image-differential` with a record of `python3 -B crates/filesystem/hfsplus-builder/verification/check-canonical-volume.py --interleaved-forks`. The independent Python reader checks one deterministic 8 MiB image, 134 catalog records, four continuation keys, and both complete fork payloads with zero block padding. Use Python 3.14.0 and Rust 1.99.1 for this release profile and set `fixture_hash` to the pinned image digest. The comparator reads an ordinary fixture file.

Use `--profile sealed-image-native` with a record of `env -u TMPDIR python3 -B crates/filesystem/hfsplus-builder/verification/check-canonical-volume.py --interleaved-fsck-only`. Set `fixture_hash` to the same pinned image digest. This macOS profile checks one clean, nonjournaled, generated volume with Apple `fsck_hfs` 650.140.2 on a temporary read-only software device. The script verifies the device is unmounted, requires successful `-fn`, detaches it, and checks that the entire image digest stayed identical. The scope is filesystem consistency for that fixture. [Native verifier](../crates/filesystem/hfsplus-builder/verification/readonly_fsck_oracle.py)

The bundle is a JSON array of [schema v1 records](evidence.schema.json). Set `commit` to the exact Git commit and `source_hash` to the pinned archive SHA-256 above. Record the policy's feature set, command and bounds, the observed tool versions, the runner, elapsed time, and invocation exit code. For Kani records, use the profile name for `proof.theorem`. Include the invocation log as an artifact named `transcript`, with its SHA-256 and a path relative to the bundle.

The checker binds the record to the committed source and policy and verifies the artifact bytes. It checks the exact output each profile requires, including Kani covers, completed Rust test cases, or the differential fixture digest and readback. The release process provides transcripts from real invocations. These profiles and the standalone checker tests provide partial H-VER-02-T01 evidence. The HFS+ checker is adapted from Pomologie APFS commit `62877b6` and runs entirely within this repository. [Checker tests](../tools/test_check_release_evidence.py)

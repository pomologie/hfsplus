# HFS+ builder verification

The Kani placement harness covers the interval admission portion of H-V07. It checks up to eight placements with arbitrary `u64` offsets and lengths against an independent widened-arithmetic prefix model. The harness calls the production admission function. Hosted tests exercise exact reads across boundaries, large zero ranges, nonseekable sinks, source faults, and cancellation. Native image construction and the remaining H-V07 evidence are tracked in the repository obligation registry.

The Kani allocation harnesses cover part of H-V06 for volumes with up to 32 blocks, 512-byte and 4096-byte allocation sizes, and any partial device tail below one block. An exact initial reservation can reach any admitted cursor. From that state the transition harnesses compare a full-width `u64` request and resulting free count against an independent `u128` range model. The bitmap harnesses compare every encoded bit at any admitted cursor to the same model. Larger bitmaps and complete native volumes have separate test evidence.

Install Kani 0.68.0 and run `cargo kani setup` before running `bash check.sh` from this directory.

## Canonical raw HFS+ image

The [H-BUILD-05-T01 and T02 fixture](../tests/canonical_volume.rs) emits an 8 MiB clean nonjournaled HFS+ volume with fixed geometry and HFS dates, numeric ownership, and modes. Its synthetic app contains a Unicode guide, two names for one regular file, an inline attribute on that shared file, another inline attribute on the guide, a resource fork, and a relative symlink. The fixture never executes the synthetic app. Reversed manifest, link-name and attribute order must preserve the entire volume.

The hosted suite compares both outputs. The [opt-in verifier](check-canonical-volume.py) runs the producer in separate processes and checks each output against the same pinned SHA-256 on every host. Run it with Python 3.11 or later.

```sh
python3 crates/filesystem/hfsplus-builder/verification/check-canonical-volume.py
```

Pass `--seven-zip 7z` to check the decomposed catalog spelling and exact Unicode file data, symlink bytes, and backing link inode data with an independent reader. Pass `--native-fsck` on macOS to wrap the fixture in a temporary UDRO image and run `fsck_hfs -fn` on a read-only attachment. The check then mounts the same image read-only and compares the native total and free allocation bytes with the on-disk header.

On macOS 14.7.2 arm64 (23H311) with Rust 1.99.1, both images from `a1b870f` had SHA-256 `63e2f4d50df05f59a1e6dbe63c77e5d75b10e1e2780bae068af3eec4faeadba6`. p7zip 17.05 matched the three readbacks. `fsck_hfs` reported a raw-device permission warning, completed its no-write checks, and found the volume consistent. The mounted volume matched the header's 2,048 total and 2,033 free 4,096-byte blocks. The normalized native log has SHA-256 `af1e13ce08d5eaaa8eeef4522bd4f1279e4937a552eb982034ec265fe533afc1`. [Acceptance catalog](../../../../docs/reference/test-catalog.json)

The same revision passed the placement proof and all four allocation proofs with Kani 0.68.0 and CBMC 6.11.0. The 42-test hosted builder run on nightly `2026-11-04` left three opt-in cases out of its coverage map. The 14 mapped source files cover 1,921 of 2,122 lines and 160 of 182 branch outcomes. `allocation.rs` and `plan.rs` each cover 12 of 12 reported branch outcomes. `stream.rs` covers four of six. The proof, native, and coverage records are in `target/verification-evidence/a1b870f-builder-audit/`. The `builder-coverage.json` SHA-256 is `9bcee4277faa45431d45c1f93530444657db862c97d31470d449724aebb261da`.

The stream tests now exercise cancellation before and after the second source read. Only the first completed chunk reaches the sink in either case. The sequential-reader test verifies that a failed exact read retains its cursor, a retry reads the same range, and reads at EOF leave the source untouched. All 44 builder tests and strict sealed-plan Clippy passed. Nightly `2026-11-04` measured 42 of 42 lines and six of six branch outcomes in `stream.rs` in the sealed-plan suite. The report is `target/verification-evidence/26f39cc-stream-guards/stream-coverage.json` with SHA-256 `354f8321ba13fe14343d3c1d908a0c7408eca58535561fdc070a486cbcdc29d5`. [Sealed-plan tests](../tests/sealed_plan.rs)

Each test failed under a corresponding mutation in a disposable checkout. One bypassed cancellation before a read, and the other advanced the reader before a failing read completed. The `mutations.json` record beside the coverage report has SHA-256 `bf1516295f4cf80bb528cbbe5dcfb678f6d009262fa4b4f02101c3bf238a8ee6`.

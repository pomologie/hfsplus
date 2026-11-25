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

Pass `--seven-zip 7z` to check the decomposed catalog spelling and exact Unicode file data, symlink bytes, and backing link inode data with an independent reader. Pass `--native-fsck` on macOS to wrap the fixture in a temporary UDRO image, attach it read-only without mounting and run `fsck_hfs -fn` on the attached image.

On macOS 14.7.2 arm64 (23H311) with Rust 1.99.1, both images from `f727033` plus this fixture had SHA-256 `63e2f4d50df05f59a1e6dbe63c77e5d75b10e1e2780bae068af3eec4faeadba6`. p7zip 17.05 matched the three readbacks. `cargo check` passed for the witness on Linux GNU and Windows MSVC targets. The local sandbox's `hdiutil attach` failed with `Device not configured`. Native fsck, mounted metadata checks, and independent Linux and Windows host runs remain open. [Acceptance catalog](../../../../docs/reference/test-catalog.json)

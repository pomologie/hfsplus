# hfsplus

An independent Rust implementation of the HFS+ and HFSX filesystems.

The initial `hfsplus-format` crate decodes volume headers, embedded fork extents, B-tree nodes and headers, and catalog folder, file and thread records. It preserves raw UTF-16 name units and catalog metadata. It builds without `std` or an allocator.

Native fixtures from macOS 14.7.2 cover a clean partitionless volume and a regular file in an Apple-generated optical HFS+ volume. [Fixture provenance](tests/fixtures)

`hfsplus-builder` can stream clean, nonjournaled HFS+ volumes from manifests of printable ASCII paths, directories, and files with data and resource forks. Its [crate-local verification](crates/filesystem/hfsplus-builder/verification) covers image placement admission.

```sh
cargo nextest run --locked
cargo build --locked -p hfsplus-format --no-default-features --target aarch64-unknown-none
cargo build --locked -p hfsplus-format --no-default-features --target x86_64-unknown-none
```

The [architecture](ARCHITECTURE.md) and [roadmap](ROADMAP.md) track the complete raw reader, historical name collation, and broader image construction profiles for dmgez.

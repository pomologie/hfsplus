# hfsplus

An independent Rust implementation of the HFS+ and HFSX filesystems.

The initial `hfsplus-format` crate decodes volume headers, embedded fork extents, B-tree nodes and headers, and catalog folder, file and thread records. It preserves raw UTF-16 name units and catalog metadata. It builds without `std` or an allocator.

Native fixtures from macOS 14.7.2 cover a clean partitionless volume and a regular file in an Apple-generated optical HFS+ volume. [Fixture provenance](crates/formats/hfsplus-format/tests/fixtures)

`hfsplus-builder` can stream clean, nonjournaled HFS+ volumes from Unicode manifests of folders, symbolic links, and files with data and resource forks. `volume::plan` writes Unicode 3.2 canonical names and uses historical HFS+ collation for catalog order and collisions. `volume::plan_ascii` limits inputs to printable ASCII.

On macOS 14.7.2 build 23H311, Apple’s read-only checker accepted a synthesized multilevel catalog with 210 files. A read-only mount exposed all payloads and resolved the tested accent, ignorable-character, and historical case aliases. A separate mount reported the full 255-unit CJK volume name. Some supplementary decompositions are addressable on this system by their canonical catalog spelling only. The builder rejects the NUL presentation symbol U+2400 and the BMP noncharacters U+FFFE and U+FFFF because macOS did not expose them as round-trippable names. [Unicode rules and provenance](crates/foundation/hfsplus-core/UNICODE32.md)

The builder’s [crate-local verification](crates/filesystem/hfsplus-builder/verification) covers image placement admission.

```sh
cargo nextest run --locked
cargo build --locked -p hfsplus-format --no-default-features --target aarch64-unknown-none
cargo build --locked -p hfsplus-format --no-default-features --target x86_64-unknown-none
```

The [architecture](ARCHITECTURE.md) and [roadmap](ROADMAP.md) track the complete raw reader and broader image construction profiles for dmgez.

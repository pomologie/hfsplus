# hfsplus

An independent Rust implementation of the HFS+ and HFSX filesystems.

The initial `hfsplus-format` crate decodes volume headers, embedded fork extents, B-tree nodes and headers, and catalog folder, file and thread records. It preserves raw UTF-16 name units and catalog metadata. It builds without `std` or an allocator.

Native fixtures from macOS 14.7.2 cover a clean partitionless volume and a regular file in an Apple-generated optical HFS+ volume. [Fixture provenance](crates/formats/hfsplus-format/tests/fixtures)

`hfsplus-reader` checks the allocation bitmap, catalog hierarchy, and reachable file forks. It counts shared regular-file inodes once and reports unclaimed allocations and unsupported record layouts. [Native structural-check fixtures](crates/filesystem/hfsplus-reader/tests/check.rs)

The reader selects binary UTF-16 ordering or HFS+ case folding from the HFSX catalog header. Native checks confirmed distinct case variants on binary volumes and equivalent case variants on a case-folded volume. The binary fixtures include a verified empty journal. Unknown HFSX comparison types report an unsupported profile. [Binary HFSX evidence](crates/filesystem/hfsplus-reader/tests/fixtures/native-hfsx-case-sensitive.json) [Case-folded HFSX evidence](crates/filesystem/hfsplus-reader/tests/fixtures/native-hfsx-case-folded.json)

The reader opens inline and fork-backed attributes, including fragmented values with continuation records. Its optional `decmpfs-zlib` feature exposes logical bytes for native inline and resource-fork zlib profiles, including regular hard links. Stored forks remain separately accessible. [Native attribute evidence](crates/filesystem/hfsplus-reader/tests/fixtures/native-attribute-forks.json) · [Native compression evidence](crates/filesystem/hfsplus-reader/tests/fixtures/native-decmpfs-zlib.json)

`hfsplus-builder` can stream clean, nonjournaled HFS+ volumes from Unicode manifests of folders, symbolic links, and files with data and resource forks. `volume::plan` writes Unicode 3.2 canonical names and uses historical HFS+ collation for catalog order and collisions. `volume::plan_ascii` limits inputs to printable ASCII.

`volume::plan_with_hard_links` gives each visible file path its own catalog ID. All paths in a regular-file hard-link group share a private inode, data and resource forks, and inline attributes. Apple’s read-only checker and a read-only mount verified two such groups, including link counts, modes, timestamps, and exact fork and attribute bytes, on macOS 14.7.2 build 23H311. [Native format evidence](crates/formats/hfsplus-format/tests/fixtures/native-hard-links.json)

On macOS 14.7.2 build 23H311, Apple’s read-only checker accepted a synthesized multilevel catalog with 210 files. A read-only mount exposed all payloads and resolved the tested accent, ignorable-character, and historical case aliases. A separate mount reported the full 255-unit CJK volume name. Some supplementary decompositions are addressable on this system by their canonical catalog spelling only. The builder rejects the NUL presentation symbol U+2400 and the BMP noncharacters U+FFFE and U+FFFF because macOS did not expose them as round-trippable names. [Unicode rules and provenance](crates/foundation/hfsplus-pomologie-core/UNICODE32.md)

The foundation Cargo package is `hfsplus-pomologie-core`. Its Rust library name is `hfsplus_core`.

The builder’s [crate-local verification](crates/filesystem/hfsplus-builder/verification) covers image placement admission, bounded allocation transitions, and bitmap accounting.

```sh
cargo nextest run --locked
cargo build --locked -p hfsplus-format --no-default-features --target aarch64-unknown-none
cargo build --locked -p hfsplus-format --no-default-features --target x86_64-unknown-none
```

The [architecture](ARCHITECTURE.md) and [roadmap](ROADMAP.md) track the complete raw reader and broader image construction profiles for dmgez.

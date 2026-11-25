# hfsplus architecture

## Purpose and independence

Own a native, portable HFS Plus/HFSX engine. Initial customer dmgez needs accurate read access and deterministic write-once construction, with direct virtual-sector output and Mac metadata fidelity. Future Charles needs independently reusable storage code, eventually mutable operations and journaling. This repository accepts raw volume images, not DMG files; it knows nothing about UDIF, Finder window design, dmgez configuration or Charles APIs. Existing fstool and hfsplus crates are differential references only. The initial filesystem profile is clean, nonjournaled HFS+ data volume, no classic HFS wrapper output. Support HFSX in a separately qualified read/build profile; later classic HFS if independently justified (not a default goal). Full read/write claims require native behavior tests, not just reading our own images. Apple TN1150 (S07) documents the baseline on-disk format; later macOS features require additional evidence. The plan deliberately separates a bulk formatter from a mounted writer so a complete mutable engine is not on dmgez's critical path.

## Workspace (mature target)

```text
hfsplus/
  apps/hfsplus-cli/             # inspect/check/build raw images, later maintenance
  crates/
    foundation/hfsplus-pomologie-core/    # geometry, I/O, names, metadata, typed errors, budgets
    formats/hfsplus-format/     # on-disk codecs, volume/fork/B-tree record definitions
    filesystem/hfsplus-reader/  # immutable view and consistency inspection
    filesystem/hfsplus-builder/ # deterministic image plan and sector source
    filesystem/hfsplus-writer/  # later mutable engine, journal module, transaction API
  tests/fixtures/  tests/differential/  tests/compatibility/
  benches/  fuzz/  examples/  docs/  xtask/  .github/workflows/
  Cargo.toml  Cargo.lock  rust-toolchain.toml  AGENTS.md
```

Only instantiate crates when implementation starts. `format -> core`; reader and builder depend on core + format, may share factored semantic record/validation modules in format; writer depends on core/format/read-only traversal helpers, not builder. Apps compose features. No reader dependence in production bulk builder unless genuinely required and reviewed; integration tests use reader independently. No reverse dependency from foundational crates to apps, platform binding, host filesystem or dmgez. Public API types separate raw record bytes from semantic model. Use fully prefixed packages, check registry availability before publishing; no assumption `hfsplus` facade or any specific prefix is available. All reference tools development-only. `no_std + alloc` mandatory for core/format/reader and future writer/journal from their first release (prefer core-only fixed-width codecs); builder may use std, shared kernel algorithms may not. Host adapters are a separate additive std feature; no mandatory host time, threads or system path dependencies. EES-1 §3a defines mandatory bare-metal compile gates. `ReadAt`/`MutableDevice` contract C2, library-local traits with minimal error translation. `ReadAt` source may represent bounded partition at offset zero; block size is filesystem geometry independent of device transport; raw file/memory/in-memory test adapters. Construction and read-only mount are independently useful without filesystem modification.

## Interfaces (proposed, not final Rust syntax)

```rust
// HFS+ library owns each trait and error type. dmgez implements adapters.
Volume::open(source, ReadOptions) -> Result<Volume>
Volume::root(), lookup(parent, name), read_dir(node, page), stat(node)
Volume::open_fork(node, ForkKind), read_link(node), list_attrs(node), open_attr(node, key)
Volume::inspect(InspectOptions) -> Report
Volume::check(CheckOptions) -> VerificationReport
Builder::plan(tree, GeometryPolicy, MetadataPolicy) -> Result<PlanDraft>
PlanDraft::identity(entry), bind_generated_payload(entry, source), seal() -> SealedPlan
SealedPlan::len(), read_exact_at(offset, buf), write_to(sink), ranges()
```

Builder consumes immutable logical tree and stream handles, no host paths required. Owns stable catalog node IDs (CNIDs), normalization/comparison, space layout and special metadata; dmgez adds generated Finder bytes via reserved entries in draft. Seal freezes every extent and size; reads are exact and concurrent only with thread-safe sources. Raw image writer can run without reading any DMG. `Volume` is immutable at a specific source identity; private IDs and user path lookup distinct. Link object vs resolved referent explicit. Expose capability/report types with unknown vs unsupported vs failed, raw metadata bytes optional and bounded. All public limits documented. No unsafe untrusted struct casts, no kernel assumptions in format engine. Later `WritableVolume` separate type, explicit exclusive device and transaction lifetime.

## Feature boundaries

Core/format/reader/writer no std dependency is an enforced kernel-profile requirement. `read`, `build`, `std`, `hfsx` separately exposed through appropriate packages; `check` reader-based; `decmpfs-zlib`, `decmpfs-lzvn`, `decmpfs-lzfse` optional reader functionality; `mutate` off until implemented, `journal` includes mutation and explicit replay support when qualified, `resize` later. `hfsplus-cli` defaults `std,read,build,check` for raw image usefulness; consumers disable defaults and select only required capabilities. Default dmgez builder uses only standalone build + required metadata support. Fuzz parser in test builds regardless of prod CLI's defaults. Unknown compression/dirty journal must yield typed unsupported for affected logical reads, not empty content. No classic HFS implementation in first release; recognize wrappers only if evidence-backed bounded subvolume.

## Compatibility contract

First release supports HFS+ volume headers, allocation map, special files, catalog/thread/extent/attr trees, clean volumes, file/folder metadata, hard-linked regular files, symlinks, both forks, xattrs. Read exact builder subset and native-generated examples with same capabilities. Clean read-only support expands to HFSX case-sensitive and case-insensitive variants, fragmented extents, journal-aware read policy, decmpfs types by profile, directory hard links only after validation. Core must use historical filesystem-specific normalization/collation not host locale. More in `docs/features/FORMAT.md`, `BUILD.md`, `READ.md`, `MUTATION.md`. C3 metadata identity handshake ensures Finder aliases are generated with final volume values. Strong compatibility gate: native fsck check (read-only) and mounting plus tree/metadata oracle, independent readers, malicious inputs, repeatable raw bytes for canonical manifest. No Apple tools required for actual library use. Report precise limitations of metadata preservation and timestamp range. Charles driver later requires real block device/barrier semantics, concurrency testing, crash replay and kernel integration, not simply no_std compilation.

## Verification integration

The local `VERIFICATION.md` and `verification/obligations.json` define mandatory profile-specific verification gates in addition to the original acceptance tests. Apply ecosystem standard EVS-1 (copy `shared/VERIFICATION.md` as `docs/architecture/ECOSYSTEM_VERIFICATION.md`). No required proof, model check, native test or critical fault gate can be substituted by coverage or a self-generated round trip. `cargo-nextest`, property tests, fuzz/corpus replay, coverage and deterministic architecture checks are required; formal methods activate at the milestones in the obligation registry.

# hfsplus roadmap

All stages are proposed, test-gated releases, not calendar estimates. Use independent package versions; dmgez pins a qualified build/read release, Charles is future consumer. Do not wait for a complete filesystem before releasing the initial standalone constructor.

| Release | Scope | Exit evidence | Blocks dmgez? |
|---|---|---|---|
| 0.1.0 | Format codecs, names, raw reader, clean nonjournaled HFS+ bulk constructor, regular hard links, forks/attrs, stable ID draft + sealed plan | H-FMT-01..04, H-BUILD-01..05, H-READ-01; native fsck/mount and tree metadata comparisons, raw serialization & sector streaming, differential tests | Yes, initial builder |
| 0.2.0 | Expanded reader/checker, HFSX profiles, overflow/extents, decmpfs zlib, clean journaled read detection, internals | H-READ-02..04, profile matrix and unsupported diagnostic tests | dmgez deep inspect |
| 0.3.0 | Qualified additional compressed reads (LZVN/LZFSE as needed), directory hard link/wrapper read profiles, performance | Profile-specific fixtures and independent checks; no empty fallback | Optional |
| 0.4.0 | Mutable filesystem engine in disposable raw images, transaction API and mutation invariants | H-MUT-01, native readback and failure injection | No |
| 0.5.0 | Journal transactions/replay and crash consistency | H-MUT-02, cut-point tests and native fsck | No |
| 0.6+ | Safe resize, broader metadata, VFS adapter qualification | H-MUT-03 and device tests | No |
| 1.0.0 | Stable documented supported profiles/API, fuzz and compatibility gates, release/dependency policy | All advertised capabilities pass; optional Charles-specific integration can remain later | No |

Bootstrap: acquire lawful synthetic fixtures; implement checked geometry and endian codecs; volume/catalog/extent reader independent of builder; normalization table qualification; bulk tree packing and range planning; metadata/forks and stable identities; dmgez adapter. Keep first builder scope strict and small. If a hard link/xattr profile isn't ready, block 0.1 release of that advertised profile rather than claim support or flatten metadata. No feature flags for unimplemented functionality in published manifest. Initial raw CLI may be helpful for development but never required by dmgez. See the [testing guide](TESTING.md) and [test catalog](../reference/test-catalog.json).

## v0.1.0 source packages

The dependency order from the normalized Cargo manifests is `hfsplus-pomologie-core` and `hfsplus-format` first, then `hfsplus-reader`, then `hfsplus-builder`. The builder archive retains its versioned reader development dependency for tests. The four exact package names returned 404 from `cargo owner --list` during the candidate check.

At `4e2adf5`, Cargo 1.99.1 assembled all four archives. Its built-in workspace verifier stopped on a missing checksum for an unpublished sibling. Running the locked package assembly with `--no-verify` and testing the extracted archives in a fresh offline workspace passed 197 tests, with ten opt-in tests skipped. Each archive's recorded Git commit and included license files were checked. Package checksums, test output, and the dependency order are in `target/release-candidates/v0.1.0/4e2adf5-cargo/`. The acceptance and obligation gates apply separately from package assembly.

## Verification integration

The [verification guide](VERIFICATION.md) and [obligation index](../../verification/obligations.json) define mandatory profile-specific verification gates in addition to the original acceptance tests. Apply [ecosystem standard EVS-1](ECOSYSTEM_VERIFICATION.md). No required proof, model check, native test or critical fault gate can be substituted by coverage or a self-generated round trip. `cargo-nextest`, property tests, fuzz/corpus replay, coverage and deterministic architecture checks are required; formal methods activate at the milestones in the obligation registry.

## Cross-cutting release gate: H-PORT-01
Beginning at 0.1.0, every newly introduced kernel-facing crate must pass EES-1 §3a on both provisional bare-metal targets and preserve hosted semantic parity. Apply incrementally to later reader/writer/transaction features; std is allowed in standalone bulk builders and adapters. Actual Charles driver qualification remains a later milestone.

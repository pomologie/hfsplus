# Repository handoff and integration

## Import

The three project directories are merge-ready documentation trees, not generated Rust repositories. Copy each into the corresponding existing repo after inspecting its README, licensing, AGENTS and CI. Do not overwrite existing source, history, licenses, or user changes. Shared contracts and references are maintained in this pack; copy `shared/CONTRACTS.md` and `shared/REFERENCES.md` into each repository as `docs/architecture/ECOSYSTEM_CONTRACTS.md` and `docs/reference/ECOSYSTEM_REFERENCES.md` (record the same revision). Also copy `shared/VERIFICATION.md` into each repo as `docs/architecture/ECOSYSTEM_VERIFICATION.md`. Each repo has its own `docs/architecture/VERIFICATION.md`, repository-wide `verification/obligations.json` and evidence schema. Retain the obligation IDs when wiring crate-owned verification. Copy `shared/HANDOFF.md` and `shared/REVIEW.md` if useful. Keep the pack-level validator and abstract models as documentation validation examples, not shipped runtime code. Relative links in pack-level docs are navigation within the pack. Repo-level docs cite contract IDs and source IDs to remain readable after copying. Keep requirements/test IDs stable; no cross-repo Git submodules required. Do not add a fourth library just to share a two-method trait.

## Bootstrap order and first work packages

| ID | Owner | Work item | Exit artifact |
|---|---|---|---|
| BOOT-01 | all | Establish workspace, toolchain policy, license decisions, dependency audit, feature CI | Empty/minimal builds on supported targets; no placeholder capability flags |
| DM-01 | dmgez | Collect native UDRO/UDZO fixtures from synthetic raw data; document exact block/run/checksum profiles | Fixture manifests, reference dumps, semantic expected outputs |
| HF-01 | hfsplus | Build raw volume fixture generator and metadata oracle; geometry/newtypes/name rules | Native synthetic images, expected tree and metadata |
| DM-02 | dmgez | Implement minimal fixed-endian koly + blkx parser/writer; safe bounds and CRC qualification | UDRO wraps deterministic raw bytes; macOS attach/verify |
| HF-02 | hfsplus | Parse header, catalog/extents and tree; complete raw reader for supported builder subset | Independent read of Apple-generated fixtures |
| HF-03 | hfsplus | Implement manifest plan with stable CNIDs, bulk B-tree construction, allocation bitmap, forks | Emit raw volume that passes native fsck and semantic manifest comparison |
| DM-03 | dmgez | UDZO serial, bounded block indexing and random-access reader | Mac and independent round-trip/differential fixtures |
| INT-01 | dmgez + hfsplus | Adapt sealed HFS+ plan to UDIF input; preserve exact metadata | A real image produced on all 3 OSes and mounted on macOS |
| DM-04 | dmgez | Add bounded Rayon, cancellation, ordered writer, budget benchmark | Byte-equivalent unsigned serial/parallel outputs for same profile |
| DM-05 | dmgez | TOML/manifest, arbitrary contents, Finder aliases and layouts | Multi-app installer fixture, dry-run, metadata integrity |
| AP-01 | apfs (parallel, nonblocking) | Source/license register and read-only container fixtures; exact version/feature inventory | Evidence ledger and parser safety tests; no write promises |

First build integration must not wait for APFS, full HFS mutable operations, GUI, signing, or mount. Preserve release independence: every hfsplus/apfs release tests its own raw images without dmgez; dmgez integration tests pin a tested library release and fixture contract. Later release upgrades rerun native tests. Start only crates with real responsibilities. Create package manifests once code exists; the planned graph is not a request for 25 empty crates.

## Suggested PR sequence

1. Add format fixture collection scripts and traceability, including synthetic data creation and source permission notes. Fail CI if required oracle isn't available in a required compatibility job; mark explicitly skipped in non-oracle jobs.
2. Add fixed-width codecs with checked slices, no untrusted struct casting. Test boundary values before accepting any image. Never use the same writer as the only reader oracle.
3. Add read/write profiles incrementally; label unsupported feature combinations explicitly. No broad filesystem/format support badges until all profile tests pass.
4. Add shared services before CLI/GUI/MCP surface; keep actual commands thin. Add integration contracts and capability diagnostics before optional feature promotion.
5. Stabilize schema/versioning, signed/reproducible profiles, benchmarks and native test evidence. Publish only after release gates pass, not because the milestone date arrived.

## Policy for agents

General coding rules belong in `AGENTS.md`; specific feature behavior belongs in the appropriate feature spec. Read `docs/architecture/ARCHITECTURE.md`, current roadmap and owning feature document before implementation. For ambiguous binary fields, produce a minimal fixture and resolve against independent evidence; document the result rather than making an unsupported assumption. A proposed plan is never evidence of current code. When adapting to existing source, compare actual repository state first and preserve working implementations unless an explicit change is warranted.

## Verification bootstrap

Before format code: install pinned nextest/property/fuzz/coverage/dependency lanes; create a checked-range Kani pilot in the owning crate’s `verification/kani/` and create native fixture manifests. In parallel qualify the hax/Charon/Aeneas extraction pipeline on one pure helper. Implement deterministic fault-device/read counter adapters, reference state models and the architecture checker as first-class tasks. Use the local obligation registry and EVS-1; before any release activate every required proof/test for its exact advertised profile. Never mark this pack as evidence of actual filesystem correctness.

## Mandatory coordinated development

Apply [EES-1](ECOSYSTEM_ENGINEERING.md) (repo copy: `docs/architecture/ECOSYSTEM_ENGINEERING.md`) in all three repositories. Import that file alongside EVS-1. All tasks start with discovery and impact assessment of dmgez, hfsplus and apfs regardless of editor workspace. Keep separate releases and validate local and released dependency combinations.

## Revision 3 kernel portability bootstrap
Before implementing filesystem crates, read EES-1 §3a, establish each no_std engine profile and add the pinned bare-metal target matrix. Apply gates from the first release of each affected crate. Test hosted adapters independently; do not wait for Charles kernel or full APFS to deliver dmgez. The master pack includes repository-local copies of shared policies, so merge each project directory directly after reviewing existing files.

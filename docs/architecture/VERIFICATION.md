# hfsplus verification plan

Treat the standalone immutable constructor and later mutable filesystem as different assurance profiles. A formatter can ship without journal mutation. The first release proves/checks geometry, packing and allocation bounds, verifies a real Apple-compatible raw image, and supports dmgez streaming. The future Charles profile needs real kernel and persistence qualification, not just no_std.

Read the [shared ecosystem verification standard EVS-1](ECOSYSTEM_VERIFICATION.md). The repository-wide [obligation registry](../../verification/obligations.json) records properties, assumptions, bounds, negative witnesses, required methods, and status. Proof artifacts live in the owning crate’s `verification/` directory. Each activated profile requires its listed evidence before release.

## v0.1.0 coverage

Run `python3 -B tools/measure_coverage.py --output "$evidence_dir"` from a clean checkout. The collector uses `nightly-2026-11-04`, its LLVM tools, and Nextest 0.9.151. It runs the locked no-default-features workspace suite and binds the source and counts from each owned production file. It reports only observed LLVM branch outcomes. [Coverage collector](../../tools/measure_coverage.py) · [EVS-1 coverage criteria](ECOSYSTEM_VERIFICATION.md#7-ci-and-quantitative-gates)

On arm64 macOS, the clean `ec8eb67` run passed 194 tests and skipped ten. LLVM covered 6,944 of 7,850 lines (88.46%) across 65 owned files. The run merged 276 profiles from 41 test binaries and excluded three build profiles. The report is in `target/verification-evidence/v0.1.0/ec8eb67-critical-coverage/` with SHA-256 `ef107bb3c00dc08af32d04d849e8a701dc6b86c6b2eb36d043d60e6576ce77ea`.

| Critical source | Lines | Branch outcomes |
|---|---:|---:|
| Builder allocation | 236 / 253 | 12 / 12 |
| Sealed plan | 163 / 169 | 13 / 14 |
| Sealed streaming | 42 / 42 | 6 / 6 |
| Reader allocation | 189 / 222 | 23 / 24 |
| B-tree format | 142 / 149 | 38 / 58 |
| Volume format | 184 / 190 | 25 / 32 |
| Name admission | 100 / 111 | 18 / 18 |
| Name collation | 27 / 27 | 2 / 2 |

H-VER-01-T01 remains open. Five of these files fall below at least one proposed critical-file threshold. The coverage run supplies source counts for the no-default-features profile. The native fixture, fuzz, and safety-invariant checks retain their own evidence.

## Obligations

| ID | Activation | Scope | Required evidence |
|---|---|---|---|
| H-V01 | 0.1.0 | geometry/codecs | kani, property, fuzz, differential |
| H-V02 | 0.1.0 | catalog/extent/attributes | kani, property, fuzz, differential, native |
| H-V03 | 0.1.0 | names | property, differential, native |
| H-V04 | 0.1.0 | metadata/forks/links | property, fuzz, differential, native |
| H-V05 | 0.1.0 | stable IDs | property, state-machine, differential, native |
| H-V06 | 0.1.0 | fresh image allocation | kani, property, differential, native |
| H-V07 | 0.1.0 | sealed image | kani, property, fault, differential, native |
| H-V08 | 0.2.0 | B-tree lookup | kani, property, fuzz, differential |
| H-V09 | 0.2.0 | compressed/profile read | property, fuzz, differential, native |
| H-V10 | 0.3.0 | geometry/packing proof | lean-linked, property, differential |
| H-V11 | 0.4.0 | mutable operations | state-machine, property, fault, native |
| H-V12 | 0.5.0 | journal commit/replay | tlc, fault, state-machine, native |
| H-V13 | 0.6+ | resize and device integration | kani, property, fault, native |
| H-V14 | 1.0.0 | stable supported engine | lean-linked, property, mutation, differential |

## Invariant and proof details

### H-V01 — geometry/codecs
**Target:** hfsplus-format / header, fork and region arithmetic.\
**Property:** Accepted geometry, block size/count and fork ranges fit exact bounded raw image; no integer overflow or out-of-range indexing.\
**Assumptions and initial bounds:** Kani full u64 values, arrays up to 8 extents, arbitrary truncated byte buffers; native min/max admissible profiles.\
**Negative witness:** allocation block index multiplied by size overflows.\
**Requirement links:** H-FMT-01, H-READ-01.\
**Evidence:** kani, property, fuzz, differential; status planned.

### H-V02 — catalog/extent/attributes
**Target:** hfsplus-format / packed node codec.\
**Property:** Validated node offset table partitions records without overlap with descriptors/table; every parsed key and data range valid, typed and monotone under correct comparator.\
**Assumptions and initial bounds:** Kani <=8 records/128-byte synthetic packing and real nodes tested at qualified sizes; endian exact; comparator contract.\
**Negative witness:** offset table points into itself; malformed node count reads before start.\
**Requirement links:** H-FMT-02, H-BUILD-03.\
**Evidence:** kani, property, fuzz, differential, native; status planned.

### H-V03 — names
**Target:** hfsplus-pomologie-core name keys, hfsplus-builder planner, and hfsplus-reader catalog lookup.\
**Property:** Name canonicalization is idempotent on admitted names; sibling equality agrees with historical HFS+ profile; comparator antisymmetry/transitivity, sorting and lookup agree; collisions rejected.\
**Assumptions and initial bounds:** Exhaustive ordered triples for 94 kernel and 92 scalar canonical names, 4,096 seeded triples, planner ordering and 1,024 lookups across 16 exact separators. Live CoreServices checks 65,536 UTF-16 units, 40,000 seeded pairs and 76 directed pairs. One mounted 64 MiB HFS+ image supplies the native name corpus.\
**Negative witness:** generic modern Unicode folding accepts an HFS+ collision.\
**Requirement links:** H-FMT-03, H-BUILD-01.\
**Evidence:** All three configured methods passed on clean revision `84a365f` within their recorded bounds. [Historical name evidence](../../verification/README.md#historical-name-release-evidence)

### H-V04 — metadata/forks/links
**Target:** hfsplus-format, hfsplus-reader / forks and link resolver.\
**Property:** Logical fork read returns exact data within allocated coverage; forks distinct; hard-link identity and reference count consistent for admitted regular-file profile; traversal cannot cycle indefinitely.\
**Assumptions and initial bounds:** Native source metadata known, lengths representable; small reference tree <=12 nodes, extents <=8; unsupported directory links explicitly refused until qualified.\
**Negative witness:** resource fork confused with data fork; hardlink deref cycle.\
**Requirement links:** H-FMT-04, H-READ-01, H-BUILD-03.\
**Evidence:** The differential and native profiles passed on clean revision `b82a34f`. The property profile checked 4,810 in-bounds reads through five aliases in each of two retained valid fixtures on clean revision `269f9c1`. Separate fuzz evidence remains open. [Fork and metadata release evidence](../../verification/README.md#fork-and-metadata-release-evidence)

### H-V05 — stable IDs
**Target:** hfsplus-builder / draft/finalized identity registry.\
**Property:** Each live catalog object gets one valid CNID in correct namespace; parent/thread references resolve; generated asset length changes do not change frozen IDs; no reserved collisions.\
**Assumptions and initial bounds:** Finite tree <=16 entries + overflow boundaries; deterministic comparator; identities fixed before seal.\
**Negative witness:** reallocation after alias bytes generated changes CNID.\
**Requirement links:** H-BUILD-01, H-BUILD-03.\
**Evidence:** All four configured methods passed on clean revision `89df833` within their recorded bounds. [Stable-ID evidence](../../crates/filesystem/hfsplus-builder/verification/README.md#stable-id-release-evidence)

### H-V06 — fresh image allocation
**Target:** hfsplus-builder / planner and bitmap.\
**Property:** All allocated block ranges contained, disjoint for distinct owners except qualified links; bitmap and allocated/free counts equal reference sets; required reserved and metadata regions protected.\
**Assumptions and initial bounds:** Kani covers up to 32 blocks for 512- and 4096-byte block sizes and one full-width transition from any admitted cursor. Separate tests compare four positive layouts, one independent block ownership reconstruction, and one native free-count fixture.\
**Negative witness:** allocator double uses alternate-header region.\
**Requirement links:** H-BUILD-02, H-BUILD-03.\
**Evidence:** All four configured methods passed on clean revision `78ac3fc` within their recorded bounds. [Fresh allocation evidence](../../crates/filesystem/hfsplus-builder/verification/README.md#fresh-allocation-release-evidence)

### H-V07 — sealed image
**Target:** hfsplus-builder / interval plan and range producer.\
**Property:** Sealed ranges cover [0,image_len) exactly once, known zeros explicit, source data exact; arbitrary partial reads concatenate to identical sequential output, source mutation fails.\
**Assumptions and initial bounds:** Kani <=8 ranges with arbitrary u64 endpoints; immutable bounded source contract; native raw image output.\
**Negative witness:** gap implicitly zeroed over unplanned metadata; short source read accepted.\
**Requirement links:** H-BUILD-04, H-BUILD-05.\
**Evidence:** All five configured methods passed on clean revision `a895326` within their recorded bounds. [Sealed image evidence](../../crates/filesystem/hfsplus-builder/verification/README.md#sealed-image-release-evidence)

### H-V08 — B-tree lookup
**Target:** hfsplus-reader / pure B-tree descent and key search.\
**Property:** For a validated finite tree, search returns unique matching record iff present in abstract sorted map; separator routing and leaf iteration preserve total ordering; invalid links error, cycle bounded.\
**Assumptions and initial bounds:** Property/reference random histories; proof pilot later for pure packed search, not arbitrary malicious tree without validation.\
**Negative witness:** wrong boundary separator routes equal key to adjacent leaf.\
**Requirement links:** H-FMT-02, H-READ-01, H-READ-04.\
**Evidence:** kani, property, fuzz, differential; status planned.

### H-V09 — compressed/profile read
**Target:** hfsplus-reader / profile admission and decmpfs.\
**Property:** Unsupported journal/feature/compression type never masquerades as complete read; supported compressed fork exact declared size and within budget; malformed object yields error.\
**Assumptions and initial bounds:** Only individually qualified compression types, external codec contract; native type fixtures; bounded read counters.\
**Negative witness:** unknown decmpfs type returns empty file.\
**Requirement links:** H-READ-02, H-READ-03, H-READ-04.\
**Evidence:** property, fuzz, differential, native; status planned.

### H-V10 — geometry/packing proof
**Target:** hfsplus-format + hfsplus-builder / pure interval and packing helpers.\
**Property:** Actual extracted Rust checked interval construction, allocation merge and packing refine mathematical nonoverlapping cover model, preserving capacity and exact record boundaries for all representable accepted inputs.\
**Assumptions and initial bounds:** Lean natural arithmetic + machine-width conversion; extraction of pure safe helpers only; comparator/table and I/O assumptions separately tested.\
**Negative witness:** overlapping allocation accepted after arithmetic truncation.\
**Requirement links:** H-FMT-01, H-FMT-02, H-BUILD-02, H-BUILD-04.\
**Evidence:** lean-linked, property, differential; status planned.

### H-V11 — mutable operations
**Target:** hfsplus-writer / allocator and operations model.\
**Property:** Create/unlink/rename/replace/setattr preserve unrelated object bytes and catalog/tree/link/allocation invariants; no new writable profile claimed crash-consistent yet.\
**Assumptions and initial bounds:** Exclusive disposable image, <=6 objects/16 blocks, mutation history and failures; no guarantee data atomicity on arbitrary in-place overwrites.\
**Negative witness:** rename into descendant creates cycle; unlink frees still-linked file.\
**Requirement links:** H-MUT-01.\
**Evidence:** state-machine, property, fault, native; status planned.

### H-V12 — journal commit/replay
**Target:** hfsplus-writer / persistence and recovery state.\
**Property:** Metadata recovery selects admissible old/new transaction state; replay applies only committed verified records once logically; crash during replay idempotent; no referenced block wrongly free.\
**Assumptions and initial bounds:** TLC <=2 transactions/3 metadata blocks, bounded torn writes/flush/crashes; actual device order/alignment and HFS journal profile separately qualified; user data semantics separate.\
**Negative witness:** commit record durable before required journal data; replay writes arbitrary home offset.\
**Requirement links:** H-MUT-02.\
**Evidence:** tlc, fault, state-machine, native; status planned.

### H-V13 — resize and device integration
**Target:** hfsplus-writer / resize planner and adapter.\
**Property:** No live or reserved block becomes out-of-range after qualified resize; block device alignment, exclusive ownership and durable barriers enforced; failed resize/reopen reports exact state.\
**Assumptions and initial bounds:** Model <=32 blocks and two barriers; only qualified resize modes; native disposable image and no_std separate evidence.\
**Negative witness:** shrink below live extent; header changed before referenced allocation relocated.\
**Requirement links:** H-MUT-03.\
**Evidence:** kani, property, fault, native; status planned.

### H-V14 — stable supported engine
**Target:** hfsplus-format + hfsplus-reader / selected core helpers.\
**Property:** Production-linked proof of binary-search correctness and packing/free-space arithmetic against reference specs, with checked preconditions and exact type conversions.\
**Assumptions and initial bounds:** Safe sequential subset; no proof of entirety of tree mutations, journaling or hardware.\
**Negative witness:** binary search returns insertion point as equality.\
**Requirement links:** H-FMT-02, H-READ-04, H-BUILD-02.\
**Evidence:** lean-linked, property, mutation, differential; status planned.

## Qualification work packages

- Define immutable pure core APIs and source/feature traceability before implementing proof harnesses. Start with range safety and binary framing, then reference set/map and traversal models.
- Create `tests/support` fault and fixture adapters, `fuzz` targets, Kani harnesses and `verification/models` for the actual registered scopes; preserve failing seeds as regression cases.
- Carry out implementation-linked Lean extraction on the first small eligible pure function before building broader theorem obligations. Failure to extract is not a proof; record alternate backend/evidence and amend the gate by explicit review before activation.
- Before shipping an activated protocol, check finite TLA+/TLC model and replay its actions/failures against production Rust. Native fixtures and actual fault/integration checks remain mandatory.
- Evidence is committed to the CI release report, never to a prefilled pass status. The present architecture pack does not contain a tested Rust implementation.

## Kernel portability release evidence
H-PORT-01 is an additional mandatory acceptance gate for every active kernel-facing crate, not a new claim of formal proof. Retain existing proof obligations. See the [testing guide](TESTING.md) and EES-1 §3a for exact profile compilation, feature graph, parity and negative leakage checks; actual kernel qualification remains separate.

`H-PORT-01-T01` passed for `hfsplus-pomologie-core`, `hfsplus-format` and `hfsplus-reader` at `0f813b3`. The pinned Rust 1.99.1 CI matrix ran `cargo check --locked -p PACKAGE --no-default-features --target TARGET` and the corresponding `cargo build` for `aarch64-unknown-none` and `x86_64-unknown-none`. A local Rust 1.99.1 replay passed `cargo build --locked --offline --release -p PACKAGE --no-default-features --target TARGET` for all six pairs. The optional compressed reader profile belongs to the later reader milestone. The feature-boundary and semantic-parity cases retain their own acceptance records. [CI evidence](https://github.com/pomologie/hfsplus/actions/runs/45820799066)

`H-PORT-01-T02` passed locally with Cargo 1.99.1. The checker inspected `cargo tree --locked --no-default-features --target TARGET -p PACKAGE --edges normal,no-proc-macro` for the same six pairs. Core and format each resolved only themselves. Reader resolved itself, core, and format. Every package had an empty enabled-feature set. The synthetic fixture passed with a host proc macro that imports a standard-library dependency and failed when an injected feature added that dependency as a target-side library. Both target profiles exercised the fixture. The checker also rejects unexpected features on an otherwise permitted dependency. [Checker](../../tools/check_kernel_graph.py) · [Fixture tests](../../tools/test_check_kernel_graph.py)

`H-PORT-01-T03` passed on macOS 14.7.2 arm64 with Rust 1.99.1. The native catalog fixture exercised the same reader through the portable byte-slice source and a hosted `std::fs::File` adapter. Both passed the independent name and payload expectations, returned identical catalog entries including metadata, and reported the same typed lookup and directory refusals. The core, format, and reader crates compile as `no_std` in both cases. Hosted selection changes the I/O adapter. Both `cargo test --locked --no-default-features -p hfsplus-reader --test catalog` and the same command with `--features decmpfs-zlib` passed all ten tests. [Parity test](../../crates/filesystem/hfsplus-reader/tests/catalog.rs) · [Native fixture provenance](../../crates/filesystem/hfsplus-reader/tests/fixtures/native-catalog-volume.json)

`H-PORT-01-T04` passed for the same v0.1 engine source. The host-only builder and optional `decmpfs-zlib` decoder are absent from the baseline dependency graph. The all-targets, all-features Clippy check compiled both profiles on Ubuntu 24.04, macOS 15 and Windows Server 2025. On macOS 14.7.2 arm64, pinned Rust 1.99.1 passed the isolated hosted `cargo test --locked -p hfsplus-builder --no-default-features` and `cargo test --locked -p hfsplus-reader --no-default-features --features decmpfs-zlib` runs. Isolated builds of the reader with that explicit codec feature succeeded on both bare-metal targets and added only `miniz_oxide` and `adler2` to the normal package graph. These builds verify the feature boundary. The codec's full semantic release gate remains in the later reader milestone. CI exercises the codec's hosted fixture separately from the baseline workspace suite. [Hosted build evidence](https://github.com/pomologie/hfsplus/actions/runs/45820799066)

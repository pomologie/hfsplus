# Architecture review and qualification limits

**Review scope:** quality and internal coherence of the proposed architecture, not correctness of software that has yet to be built. Scores are subjective engineering-design ratings on a 10-point scale, not measurements, independent certification, legal clearance or proof-completion percentages. The user's >9/10 criterion is applied to the written plan; implementation feasibility, correctness, interoperability and performance themselves remain empirically unqualified. No score may be used as release evidence.

## Review rounds and corrections

1. Ownership and scope: removed fstool as production filesystem backend; retained focused primitives and independent oracles; separated HFS+ and APFS from DMG-specific orchestration; used domain-prefixed workspace names, separate apps and no circular dependencies. Narrowed initial writer to partitionless clean HFS+ and unsigned UDIF, made APFS and legacy nonblocking. Resolved distinction between a DMG and a writable filesystem.
2. Binary and data fidelity: separated logical and physical geometry, precise checksum domains, raw/zero/ignore controls, name normalization, fork and link metadata, stable CNID handshake for Finder aliases, bounded parser/decoder and source-change checks. Distinguished immutable fresh image planning from mutable writes and hardware durability. Defined exact support profiles instead of claiming all historical formats.
3. Platform and safety: separated VFS from OS mount; explicit sidecar metadata loss policies and path confinement, native drag/drop and eject completion gates, staging and atomic publication limits; protected credentials, optional Apple signing, honest native verification and provenance. Default no GUI/MCP/network/driver/signing/legacy. Refined cache and remote image identity.
4. Formal verification and traceability: incorporated ExaCopy and Euclid 2.0 verification layers, added 46 named property/protocol/proof obligations with assumptions, finite bounds, required methods and negative witnesses; early Kani safety gate, scheduled broader production-linked Lean, mandatory crash models and actual fault tests for writable profiles. Added explicit evidence freshness, no admitted proofs, nonvacuity, mutation checking, native release gates, coverage and deterministic architecture policies. Expanded acceptance test catalog to granular cases and supplied a runnable abstract reference model plus structural validator.
5. Final consistency pass: align default graph and milestones, recognized vs implemented format/settings, strict structural schema examples, package boundaries and release feature scope. Rerun catalog/schema/graph/model validation; preserve actual outcomes in `validation.txt` and `reference-model-results.json`. Any not-yet-tested implementation claim remains planned.

## Final design rubric

| Criterion | Design rating /10 | Basis and remaining uncertainty |
|---|---:|---|
| Architecture and dependency separation | 9.7 | Independent libraries, acyclic permitted edges, typed source contracts, default graph; actual Cargo enforcement pending |
| Scope and sequencing | 9.6 | Initial HFS+/UDIF path unblocked by APFS, GUI or mutation; independently releasable phases; engineering complexity remains significant |
| Format correctness strategy | 9.4 | Explicit byte/record/checksum/profile evidence, independent comparisons, exact errors; undisclosed UDIF and evolving APFS semantics still require experiments |
| Data and metadata fidelity | 9.5 | Strong manifest/link/fork/identity contracts, no silent loss, native validation; host provenance limitations must be measured |
| Formal verification and testing strategy | 9.8 | Conjunctive proof/test gates, source-linked Lean and bounded Kani, protocol/fault simulation, native oracles, 209 cases and 46 obligations; no production proof run |
| Security and reliability design | 9.6 | Untrusted-input/resource gates, staging, path isolation, optional crypto/signing, crash and approval states, supply-chain controls; host-specific primitives need qualification |
| Performance and memory architecture | 9.4 | Sector plan, bounded ordered scheduling, separate metadata accounting, reproducibility/cache/benchmarks; actual speed not measured |
| Cross-platform and interoperability strategy | 9.5 | Raw-image contracts and Windows/Linux/macOS matrix, native oracles, explicit backend capabilities; native behavior must be tested |
| Maintainability and agent execution | 9.6 | Domain ownership, detailed specs/roadmaps, deterministic policy, IDs and source/proof evidence; live repo state still needs reconciliation |
| Product completeness and migration path | 9.5 | Build and full applicable dmgbuild inventory, actual-image GUI, release tools, mount, automation, deliberate exclusions; deprecated behavior must be separately tested |
| Reusability for Charles and Dockhand | 9.5 | Raw independent engines, mandatory kernel-facing no_std gates, versioned integration and scoped kernel gates; hardware/VFS contracts await Charles |
| Legal/IP and dependency governance | 9.2 | Lawful fixtures, source/provenance and license audits, explicit patent/IP review gates; no professional freedom-to-operate finding |

**Lowest design score: 9.2/10.** This meets the requested threshold as a self-review of the plan only. These are not claims that every risk has been resolved. Release-quality code, Apple behavior, safety of mutable operations, extraction compatibility, available crate names, current repository state, proof tool support and performance must be established independently. No native macOS oracle or Rust implementation was executed for this documentation pack; the small reference-model checks exercise abstract design examples, not real file systems.

## Remaining risk register (hard gates rather than assumptions)

| Risk | Ownership | Required evidence / safe fallback |
|---|---|---|
| Exact UDIF checksum domain, variants, raw/zero/ignore semantics | dmgez | Native field ledger, independent reference + actual attach; narrow writer to qualified profile |
| Unicode tables, HFS+ metadata and node layout | hfsplus | Native fixture comparators and tree/metadata checker; reject ambiguous inputs |
| APFS current format/checkpoint, allocator and CoW ordering | apfs | Qualified profiles, independent fixture ledger, protocol proof + persistence tests before writes; no unsupported write |
| Metadata lost by source transport | dmgez + filesystem | Explicit sidecar/provenance, strict report/error; no invention of lost bytes |
| Native copy completion, mount identity, host atomic replacement | dmgez platform | Actual per-OS test and fault evidence; disable auto-eject or replace when guarantees unavailable |
| Tool extraction support/assumptions and dependency versions | all | Pinned early pilots, reviewed external models, no vacuous proof; alternate backend by reviewed gate change |
| Patent/legal/licensing risk | all, especially advanced APFS/Charles | Provenance, notices, counsel review before risky commercialization; no claim of clearance |
| Performance, memory budgets, native code signing/notary | dmgez | Controlled benchmarks and protected native/service tests; no marketing claim without evidence |

The first action is not implementing every roadmap item: qualify synthetic fixtures, create core range/metadata test adapters, implement UDIF UDRO and the HFS+ reader/formatter independently, integrate only when raw output passes native checks. APFS can begin read-only research in parallel. Use this review to guide changes, not to waive any proof or test gate.

## Revision 3 decision review
APFS kernel-space execution is now an explicit requirement. HFS+ and APFS kernel-facing engine crates have mandatory no_std compilation and feature-graph gates; hosted image construction remains unconstrained by that requirement. This tightens the original portability goal while avoiding a mandatory Charles kernel implementation at MVP. Additional risks: target-specific optional codecs/crypto, allocator failure behavior and synchronization contracts require actual qualification. The previous design rubric is retained as historical design assessment; no new implementation or proof evidence is asserted.

"""Validate the exact H-V03 name-oracle transcripts."""

from hfsplus_evidence_transcript import require, verify_rust_tests, verify_tool_version


TEST_START = "HFS+ name test: "
CORPUS = "historical_name_corpus_has_idempotent_keys_and_all_ordered_triples_obey_the_laws"
SEEDED_NAMES = "generated_unicode_triples_preserve_comparison_laws_and_repeated_keys"
ORDERED_BUILD = "preserves_display_units_and_catalog_order_with_stable_ids"
SEPARATORS = "separators::exact_index_separators_follow_the_first_leaf_record"
NATIVE = "kernel::mounted_names_match_normalization_collisions_bounds_and_order"
DIFFERENTIAL_TESTS = (
    "folded_comparison_matches_native_equivalence_and_order",
    "decomposed_names_match_native_hfsplus_catalogs_for_shared_unicode_cases",
    "preserves_version_boundary_exposed_by_newer_optical_oracle",
    "rejects_the_historical_georgian_collision",
)


def verify_runs(transcript, profile, preamble, harnesses, reports):
    require(profile["harnesses"] == harnesses, "wrong H-V03 harness identities or counts")
    versions = profile["tool_versions"]
    lines = transcript.splitlines()
    verify_tool_version(lines[:1], "cargo", versions["cargo"])
    verify_tool_version(lines[1:2], "rustc", versions["rustc"])
    expected_start = 2
    if profile["method"] != "property":
        verify_tool_version(lines[2:3], "Python", versions["python"])
        host = (f"macOS {versions['macos']} build {versions['macos_build']} "
                f"arch {profile['bounds']['macos_arch']}")
        require(lines[3:4] == [host], "wrong macOS version, build, or architecture")
        expected_start = 4

    starts = [(index, line.removeprefix(TEST_START)) for index, line in enumerate(lines)
              if line.startswith(TEST_START)]
    require([name for _, name in starts] == list(harnesses), "wrong H-V03 test invocations")
    require(lines[expected_start:starts[0][0]] == preamble, "incorrect H-V03 oracle evidence")
    ends = [index for index, _ in starts[1:]] + [len(lines)]
    for (start, name), end in zip(starts, ends, strict=True):
        invocation = "\n".join(lines[:2] + lines[start + 1:end])
        exact_test = profile | {"harnesses": {name: harnesses[name]}}
        output, results, summary = verify_rust_tests(invocation, exact_test)
        require(not any(output[summary + 1:]), f"unexpected result after {name}")
        headers = [i for i, line in enumerate(output) if line.startswith("---- ") and line.endswith(" stdout ----")]
        expected = reports.get(name, [])
        if not expected:
            require(not headers, f"unexpected stdout for {name}")
            continue
        require(len(headers) == 1 and output[headers[0]] == f"---- {name} stdout ----"
                and results[0] < headers[0] < summary, f"missing or misplaced stdout for {name}")
        report = [line for line in output[headers[0] + 1:summary] if line]
        require(report == [*expected, "successes:", f"    {name}"],
                f"incorrect H-V03 comparison bounds for {name}")


def verify_name_property(transcript, profile):
    bounds = profile["bounds"]
    corpus = [("Kernel", bounds["kernel_names"]), ("Unicode32Scalars", bounds["scalar_names"])]
    harnesses = {
        CORPUS: sum(count ** 3 for _, count in corpus) * 2,
        SEEDED_NAMES: bounds["seeded_triples"],
        ORDERED_BUILD: 1,
        SEPARATORS: bounds["equal_separators"],
    }
    reports = {
        CORPUS: [f"{policy} / {mode} passed {count ** 3} triples over {count} stored names"
                 for policy, count in corpus for mode in ("CaseFolded", "Binary")],
        SEEDED_NAMES: [f"HFS+ generated name triples: {bounds['seeded_triples']} at seed {bounds['name_seed']:#x}, "
                       f"up to {bounds['max_generated_scalars']} scalars and {bounds['max_name_units']} stored units"],
        SEPARATORS: [f"verified {bounds['equal_separators']} equal separators over {bounds['leaf_records']} leaf records",
                     f"HFS+ generated separator lookups: {bounds['seeded_lookups']} at seed {bounds['lookup_seed']:#x}"],
    }
    verify_runs(transcript, profile, [], harnesses, reports)


def verify_name_differential(transcript, profile):
    bounds = profile["bounds"]
    expected = [
        f"Unicode 3.2 table SHA-256: {bounds['unicode_table_sha256']}",
        f"CoreServices HFS+ model: {bounds['singleton_units']} singleton units, {bounds['overrides']} overrides, "
        f"{bounds['ignorables']} ignorables",
        f"CoreServices HFS+ samples: {bounds['seeded_pairs']} seeded pairs, {bounds['overrides']} embedded overrides, "
        f"{bounds['directed_pairs']} directed pairs, at most {bounds['max_name_units']} units, seed {bounds['seed']:#x}",
        f"CoreServices HFS+ rules SHA-256: {bounds['rules_sha256']}",
        f"CoreServices HFS+ fixture SHA-256: {bounds['fixture_sha256']}",
        f"Modern Unicode {profile['tool_versions']['unicode']} Georgian keys: {' '.join(bounds['georgian_keys'])}",
    ]
    verify_runs(transcript, profile, expected, dict.fromkeys(DIFFERENTIAL_TESTS, 1), {})


def verify_name_native(transcript, profile):
    bounds = profile["bounds"]
    expected = [
        f"Mounted HFS+ image: {bounds['image_bytes']} bytes, partitionless, detached",
        f"Mounted HFS+ names: {bounds['normalization_rows']} normalization rows, {bounds['lookup_pairs']} lookup pairs, "
        f"{bounds['length_cases']} lengths, {bounds['ordered_keys']} ordered keys, "
        f"{bounds['path_observations']} path observations, {bounds['mapped_keys']} mapped keys",
        f"Mounted HFS+ fixture SHA-256: {bounds['fixture_sha256']}",
    ]
    verify_runs(transcript, profile, expected, {NATIVE: 1}, {})


NAME_VALIDATORS = {
    "property": verify_name_property,
    "differential": verify_name_differential,
    "native": verify_name_native,
}

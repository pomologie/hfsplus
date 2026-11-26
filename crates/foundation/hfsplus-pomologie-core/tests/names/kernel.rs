//! Replays the captured mounted HFS+ behavior without mounting a volume.

use std::cmp::Ordering;

use hfsplus_core::names::{
	MAX_NAME_UNITS, NameCollation, NameError, NameFragment, NameNormalization, normalize,
	normalize_utf16,
};

use super::{NATIVE_CATALOG_ORDER, read_units};

const WITNESS: &str = include_str!("../fixtures/native-kernel-names.tsv");
const POLICY: [(&str, &str, &str); 10] = [
	("empty", "", "ENOENT"),
	("dot", ".", "EEXIST"),
	("dot-dot", "..", "EEXIST"),
	("nul", "nul\0suffix", "ffi-invalid"),
	("slash", "component/child", "created"),
	("colon", "colon:name", "created"),
	("nul-symbol", "symbol-\u{2400}", "created"),
	("noncharacter", "noncharacter-\u{ffff}", "created"),
	(
		"private-file-namespace",
		"\u{2400}\u{2400}\u{2400}\u{2400}HFS+ Private Data",
		"ENOENT",
	),
	(
		"private-directory-namespace",
		".HFS+ Private Directory Data\r",
		"ENOENT",
	),
];
const MAPPED_KEYS: [&str; 3] = ["colon/name", "noncharacter-%EF%BF%BF", "symbol-\0"];
const COMPOSED_MUSIC: &str = "\u{1d15e}";
const DECOMPOSED_MUSIC: &str = "\u{1d157}\u{1d165}";
const NATIVE_NAMES: usize = 63;
const NATIVE_PAIRS: usize = 20;
const NATIVE_LENGTHS: usize = 6;

#[test]
fn mounted_names_match_normalization_collisions_bounds_and_order() {
	let mode = NameCollation::CaseFolded;
	let mut counts = (0, 0, 0);
	let mut ordered = Vec::new();
	let mut policy = Vec::new();
	let mut mapped = Vec::new();
	for line in WITNESS.lines().filter(|line| !line.starts_with('#')) {
		let fields: Vec<_> = line.split('\t').collect();
		match fields.as_slice() {
			["normalize", input, stored] => {
				let input = read_units(input);
				let text = String::from_utf16(&input).unwrap();
				let stored = read_units(stored);
				assert_eq!(normalize(&text).unwrap().as_units(), stored, "{line}");
				assert_eq!(
					normalize_utf16(&input).unwrap().as_units(),
					stored,
					"{line}"
				);
				counts.0 += 1;
			}
			["pair", _, first, second, outcome] => {
				let first = normalize_utf16(&read_units(first)).unwrap();
				let second = normalize_utf16(&read_units(second)).unwrap();
				assert!(matches!(*outcome, "same" | "distinct"));
				let same = *outcome == "same";
				assert_eq!(
					mode.compare(first.as_units(), second.as_units()).is_eq(),
					same,
					"{line}"
				);
				assert_eq!(
					mode.key(first.as_units()).unwrap() == mode.key(second.as_units()).unwrap(),
					same,
					"{line}"
				);
				counts.1 += 1;
			}
			["length", _, input, "created", stored] => {
				let stored = read_units(stored);
				assert_eq!(stored.len(), MAX_NAME_UNITS);
				assert_eq!(
					normalize_utf16(&read_units(input)).unwrap().as_units(),
					stored,
					"{line}"
				);
				counts.2 += 1;
			}
			["length", _, input, "ENAMETOOLONG"] => {
				assert!(
					matches!(normalize_utf16(&read_units(input)), Err(NameError::TooLong)),
					"{line}"
				);
				counts.2 += 1;
			}
			["order", stored] => ordered.push(read_units(stored)),
			["policy", label, name, outcome] => policy.push((
				*label,
				String::from_utf16(&read_units(name)).unwrap(),
				*outcome,
			)),
			["policy-key", key] => mapped.push(String::from_utf16(&read_units(key)).unwrap()),
			_ => panic!("unexpected native fixture row {line:?}"),
		}
	}
	assert_eq!(counts, (NATIVE_NAMES, NATIVE_PAIRS, NATIVE_LENGTHS));
	let optical: Vec<_> = NATIVE_CATALOG_ORDER
		.lines()
		.filter(|line| !line.starts_with('#'))
		.map(read_units)
		.collect();
	assert_eq!(ordered, optical);
	for (index, first) in ordered.iter().enumerate() {
		for second in &ordered[index + 1..] {
			assert_eq!(mode.compare(first, second), Ordering::Less);
			assert!(mode.key(first).unwrap() < mode.key(second).unwrap());
		}
	}
	let policy: Vec<_> = policy
		.iter()
		.map(|(label, name, outcome)| (*label, name.as_str(), *outcome))
		.collect();
	assert_eq!(policy, POLICY);
	assert_eq!(mapped, MAPPED_KEYS);
}

#[test]
fn fragments_require_explicit_scalar_compatibility_for_optical_storage() {
	let mode = NameCollation::CaseFolded;
	let composed: Vec<_> = COMPOSED_MUSIC.encode_utf16().collect();
	let scalar_stored: Vec<_> = DECOMPOSED_MUSIC.encode_utf16().collect();
	let native = NameFragment::new(mode, COMPOSED_MUSIC).unwrap().unwrap();
	assert!(native.matches(&composed).unwrap());
	assert!(!native.matches(&scalar_stored).unwrap());
	let scalar =
		NameFragment::with_normalization(mode, COMPOSED_MUSIC, NameNormalization::Unicode32Scalars)
			.unwrap()
			.unwrap();
	assert!(!scalar.matches(&composed).unwrap());
	assert!(scalar.matches(&scalar_stored).unwrap());
}

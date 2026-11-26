use std::{cell::Cell, cmp::Ordering, collections::BTreeSet};

use hfsplus_core::names::{
	MAX_NAME_UNITS, NameCollation, NameError, NameFragment, NameNormalization, NormalizedName,
	normalize, normalize_utf16,
};
use proptest::{
	prelude::*,
	test_runner::{RngSeed, TestCaseError, TestRunner},
};

#[path = "names/kernel.rs"]
mod kernel;

const NATIVE_NAMES: &str = include_str!("fixtures/native-normalization.tsv");
const NATIVE_COLLATION: &str = include_str!("fixtures/native-collation.tsv");
const NATIVE_CATALOG_ORDER: &str = include_str!("fixtures/native-catalog-order.txt");
const COLLATION_MODES: [NameCollation; 2] = [NameCollation::CaseFolded, NameCollation::Binary];
const NORMALIZATIONS: [NameNormalization; 2] = [
	NameNormalization::Kernel,
	NameNormalization::Unicode32Scalars,
];
const PROPERTY_CASES: u32 = 4096;
const PROPERTY_SEED: u64 = 0x4846_532b;
const MAX_GENERATED_SCALARS: usize = 64;

fn read_units(units: &str) -> Vec<u16> {
	units
		.split_whitespace()
		.map(|unit| u16::from_str_radix(unit, 16).expect("fixture syntax"))
		.collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OracleComparison {
	Agreement,
	NewerOptical,
}

fn native_names() -> impl Iterator<Item = (OracleComparison, Vec<u16>, Vec<u16>)> {
	NATIVE_NAMES
		.lines()
		.filter(|line| !line.starts_with('#'))
		.map(|line| {
			let (comparison, units) = line.split_once('\t').expect("fixture comparison separator");
			let comparison = match comparison {
				"agreement" => OracleComparison::Agreement,
				"newer_optical" => OracleComparison::NewerOptical,
				other => panic!("unknown fixture comparison {other}"),
			};
			let (original, expected) = units.split_once('\t').expect("fixture units separator");
			(comparison, read_units(original), read_units(expected))
		})
}

#[test]
fn decomposed_names_match_native_hfsplus_catalogs_for_shared_unicode_cases() {
	let policy = NameNormalization::Unicode32Scalars;
	for (_, original, expected) in native_names().filter(|row| row.0 == OracleComparison::Agreement)
	{
		let original_str = String::from_utf16(&original).expect("fixture input UTF-16");
		assert_eq!(
			policy.normalize(&original_str).unwrap().as_units(),
			expected
		);
		assert_eq!(
			policy.normalize_utf16(&original).unwrap().as_units(),
			expected
		);
		assert_eq!(
			policy.normalize_utf16(&expected).unwrap().as_units(),
			expected
		);
	}
}

#[test]
fn preserves_version_boundary_exposed_by_newer_optical_oracle() {
	for (_, original, observed) in
		native_names().filter(|row| row.0 == OracleComparison::NewerOptical)
	{
		// These characters were unassigned in Unicode 3.2. Native optical
		// generation uses newer decomposition and combining-class data.
		assert_ne!(original, observed);
		for policy in NORMALIZATIONS {
			assert_eq!(
				policy.normalize_utf16(&original).unwrap().as_units(),
				original
			);
		}
	}
}

#[test]
fn canonical_order_is_stable_and_does_not_cross_starters() {
	let input = "\u{301}\u{300}\u{327}A\u{301}\u{327}B\u{327}";
	let expected = "\u{327}\u{301}\u{300}A\u{327}\u{301}B\u{327}";
	assert!(
		normalize(input)
			.unwrap()
			.into_units()
			.into_iter()
			.eq(expected.encode_utf16())
	);
}

#[test]
fn counts_utf16_after_expansion() {
	let supplementary = '𐐀';
	let decomposable = "é";
	let available_scalars = MAX_NAME_UNITS / supplementary.len_utf16();
	let valid = supplementary.to_string().repeat(available_scalars) + "a";
	assert_eq!(normalize(&valid).unwrap().as_units().len(), MAX_NAME_UNITS);
	let too_long = supplementary.to_string().repeat(available_scalars) + decomposable;
	assert!(matches!(normalize(&too_long), Err(NameError::TooLong)));
}

#[test]
fn fragments_use_admitted_case_rules_on_native_stored_names() {
	let original: Vec<_> = "00-Å".encode_utf16().collect();
	let (_, _, observed) = native_names()
		.find(|(_, input, _)| *input == original)
		.expect("native composed-name fixture");
	for (collation, expected) in [
		(NameCollation::CaseFolded, true),
		(NameCollation::Binary, false),
	] {
		let fragment = NameFragment::new(collation, "å").unwrap().unwrap();
		assert_eq!(fragment.matches(&observed).unwrap(), expected);
		let fragment = NameFragment::new(collation, "Å").unwrap().unwrap();
		assert!(fragment.matches(&observed).unwrap());
	}
}

#[test]
fn rejects_fragments_and_stored_names_outside_the_name_profile() {
	const IGNORABLE: &str = "\u{200c}";
	assert!(
		NameFragment::new(NameCollation::CaseFolded, IGNORABLE)
			.unwrap()
			.is_none()
	);
	let binary = NameFragment::new(NameCollation::Binary, IGNORABLE)
		.unwrap()
		.unwrap();
	assert!(
		binary
			.matches(&IGNORABLE.encode_utf16().collect::<Vec<_>>())
			.unwrap()
	);
	assert!(matches!(
		binary.matches(&[0; MAX_NAME_UNITS + 1]),
		Err(NameError::TooLong)
	));
	let too_long = "å".repeat(MAX_NAME_UNITS);
	assert!(matches!(
		NameFragment::new(NameCollation::CaseFolded, &too_long),
		Err(NameError::TooLong)
	));
}

#[test]
fn rejects_malformed_utf16_and_preserves_an_empty_thread_key() {
	const UNPAIRED_SURROGATE: u16 = 0xd800;
	assert!(matches!(
		normalize_utf16(&[UNPAIRED_SURROGATE]),
		Err(NameError::InvalidUtf16)
	));
	assert!(normalize_utf16(&[]).unwrap().as_units().is_empty());
}

#[test]
fn folded_comparison_matches_native_equivalence_and_order() {
	for line in NATIVE_COLLATION
		.lines()
		.filter(|line| !line.starts_with('#'))
	{
		let mut fields = line.split('\t');
		let left = read_units(fields.next().expect("left fixture units"));
		let right = read_units(fields.next().expect("right fixture units"));
		let sign: i8 = fields.next().expect("fixture order").parse().unwrap();
		assert!(fields.next().is_none());
		let expected = sign.cmp(&0);
		let collation = NameCollation::CaseFolded;
		assert_eq!(collation.compare(&left, &right), expected, "{line}");
		assert_eq!(
			collation
				.key(&left)
				.unwrap()
				.cmp(&collation.key(&right).unwrap()),
			expected,
			"{line}"
		);
		assert_eq!(
			collation.compare(&right, &left),
			expected.reverse(),
			"{line}"
		);
	}
}

#[test]
fn folded_comparison_preserves_native_catalog_order() {
	let names: Vec<_> = NATIVE_CATALOG_ORDER
		.lines()
		.filter(|line| !line.starts_with('#'))
		.map(read_units)
		.collect();
	for (index, left) in names.iter().enumerate() {
		for right in &names[index + 1..] {
			assert_eq!(
				NameCollation::CaseFolded.compare(left, right),
				Ordering::Less,
				"{left:x?} must appear before {right:x?}"
			);
		}
	}
}

#[test]
fn binary_comparison_uses_unsigned_stored_units() {
	let collation = NameCollation::Binary;
	let uppercase: Vec<u16> = "A".encode_utf16().collect();
	let lowercase: Vec<u16> = "a".encode_utf16().collect();
	let ignorable: Vec<u16> = "\u{200d}".encode_utf16().collect();
	let supplementary: Vec<u16> = "😀".encode_utf16().collect();
	const NUL: u16 = 0x0000;
	const ABOVE_SURROGATES: u16 = 0xe000;
	assert_eq!(collation.compare(&uppercase, &lowercase), Ordering::Less);
	assert_eq!(collation.compare(&ignorable, &[]), Ordering::Greater);
	assert_eq!(collation.compare(&[NUL], &lowercase), Ordering::Less);
	for stored in [
		&uppercase[..],
		&lowercase,
		&ignorable,
		&supplementary,
		&[NUL],
		&[],
	] {
		assert_eq!(collation.key(stored).unwrap(), stored);
	}
	assert_eq!(
		collation.compare(&supplementary, &[ABOVE_SURROGATES]),
		Ordering::Less
	);
}

#[test]
fn folding_composes_with_canonical_decomposition() {
	let composed = normalize("É").unwrap();
	let decomposed = normalize("e\u{301}").unwrap();
	assert_eq!(
		NameCollation::CaseFolded.compare(composed.as_units(), decomposed.as_units()),
		Ordering::Equal
	);
	assert_eq!(
		NameCollation::Binary.compare(composed.as_units(), decomposed.as_units()),
		Ordering::Less
	);
}

#[test]
fn keys_bound_input_and_preserve_stored_forms() {
	const STORED_UNIT: u16 = b'A' as u16;
	for collation in [NameCollation::CaseFolded, NameCollation::Binary] {
		let allowed = [STORED_UNIT; MAX_NAME_UNITS];
		let excessive = [STORED_UNIT; MAX_NAME_UNITS + 1];
		assert_eq!(collation.key(&allowed).unwrap().len(), MAX_NAME_UNITS);
		assert!(matches!(collation.key(&excessive), Err(NameError::TooLong)));

		let composed: Vec<u16> = "é".encode_utf16().collect();
		let stored = normalize_utf16(&composed).unwrap();
		assert_ne!(
			collation.key(&composed).unwrap(),
			collation.key(stored.as_units()).unwrap()
		);
	}
}

fn historical_inputs() -> BTreeSet<String> {
	let raw_pairs = NATIVE_COLLATION
		.lines()
		.filter(|line| !line.starts_with('#'))
		.flat_map(|line| line.split('\t').take(2).map(read_units));
	let catalog_names = NATIVE_CATALOG_ORDER
		.lines()
		.filter(|line| !line.starts_with('#'))
		.map(read_units);
	let raw_names = native_names().flat_map(|(_, original, observed)| [original, observed]);
	let max_length = "é".repeat(MAX_NAME_UNITS / 2) + "a";
	raw_pairs
		.chain(catalog_names)
		.chain(raw_names)
		.filter_map(|units| String::from_utf16(&units).ok())
		.chain([max_length])
		.collect()
}

fn repeated_name(policy: NameNormalization, input: &str) -> Result<NormalizedName, TestCaseError> {
	let stored = policy.normalize(input).unwrap();
	let original: Vec<_> = input.encode_utf16().collect();
	let from_utf16 = policy.normalize_utf16(&original).unwrap();
	let repeated = policy.normalize_utf16(stored.as_units()).unwrap();
	let rendered = String::from_utf16(stored.as_units()).unwrap();
	let from_rendered = policy.normalize(&rendered).unwrap();
	prop_assert_eq!(
		&stored,
		&from_utf16,
		"UTF-8 and UTF-16 inputs differ for {:?}",
		input
	);
	prop_assert_eq!(
		&stored,
		&repeated,
		"stored units changed after normalization for {:?}",
		input
	);
	prop_assert_eq!(
		&stored,
		&from_rendered,
		"rendered text changed after normalization for {:?}",
		input
	);
	for mode in COLLATION_MODES {
		prop_assert_eq!(
			mode.key(stored.as_units()).unwrap(),
			mode.key(repeated.as_units()).unwrap()
		);
	}
	Ok(stored)
}

fn compare_pair(
	mode: NameCollation,
	left: &[u16],
	right: &[u16],
) -> Result<Ordering, TestCaseError> {
	let sign = mode.compare(left, right);
	prop_assert_eq!(
		mode.compare(right, left),
		sign.reverse(),
		"{:?}: {:x?}, {:x?}",
		mode,
		left,
		right
	);
	let left_key = mode.key(left).unwrap();
	let right_key = mode.key(right).unwrap();
	prop_assert_eq!(
		sign,
		left_key.cmp(&right_key),
		"{:?}: {:x?}, {:x?}",
		mode,
		left,
		right
	);
	Ok(sign)
}

fn transitive(first_second: Ordering, second_third: Ordering, first_third: Ordering) -> bool {
	match (first_second, second_third) {
		(Ordering::Equal, sign) | (sign, Ordering::Equal) => first_third == sign,
		(Ordering::Less, Ordering::Less) => first_third == Ordering::Less,
		(Ordering::Greater, Ordering::Greater) => first_third == Ordering::Greater,
		_ => true,
	}
}

#[test]
fn historical_name_corpus_has_idempotent_keys_and_all_ordered_triples_obey_the_laws() {
	for policy in NORMALIZATIONS {
		let names: BTreeSet<_> = historical_inputs()
			.iter()
			.map(|name| repeated_name(policy, name).unwrap().into_units())
			.collect();
		let names: Vec<_> = names.into_iter().collect();
		assert!(!names.is_empty());
		for mode in COLLATION_MODES {
			let relations: Vec<Vec<_>> = names
				.iter()
				.map(|left| {
					names
						.iter()
						.map(|right| compare_pair(mode, left, right).unwrap())
						.collect()
				})
				.collect();
			for (first, row) in relations.iter().enumerate() {
				assert_eq!(row[first], Ordering::Equal);
				for (second, &first_second) in row.iter().enumerate() {
					for (third, &second_third) in relations[second].iter().enumerate() {
						assert!(
							transitive(first_second, second_third, row[third]),
							"{mode:?}: {:?}, {:?}, {:?}",
							names[first],
							names[second],
							names[third]
						);
					}
				}
			}
			println!(
				"{policy:?} / {mode:?} passed {} triples over {} stored names",
				names.len().pow(3),
				names.len()
			);
		}
	}
}

fn generated_name() -> impl Strategy<Value = String> {
	let historical: BTreeSet<_> = historical_inputs()
		.iter()
		.flat_map(|name| name.chars())
		.collect();
	let scalar = prop_oneof![3 => prop::sample::select(historical.into_iter().collect::<Vec<_>>()), 1 => any::<char>()];
	prop::collection::vec(scalar, 0..=MAX_GENERATED_SCALARS)
		.prop_map(|units| units.into_iter().collect::<String>())
		.prop_filter("normalized length must fit the name limit", |input| {
			NORMALIZATIONS
				.into_iter()
				.all(|policy| !matches!(policy.normalize(input), Err(NameError::TooLong)))
		})
}

#[test]
fn generated_unicode_triples_preserve_comparison_laws_and_repeated_keys() {
	let mut runner = TestRunner::new(ProptestConfig {
		cases: PROPERTY_CASES,
		rng_seed: RngSeed::Fixed(PROPERTY_SEED),
		failure_persistence: None,
		..ProptestConfig::default()
	});
	let cases = Cell::new(0);
	runner
		.run(
			&(generated_name(), generated_name(), generated_name()),
			|(first, second, third)| {
				for policy in NORMALIZATIONS {
					let first = repeated_name(policy, &first)?;
					let second = repeated_name(policy, &second)?;
					let third = repeated_name(policy, &third)?;
					for mode in COLLATION_MODES {
						let first_second = compare_pair(mode, first.as_units(), second.as_units())?;
						let second_third = compare_pair(mode, second.as_units(), third.as_units())?;
						let first_third = compare_pair(mode, first.as_units(), third.as_units())?;
						prop_assert!(transitive(first_second, second_third, first_third));
					}
				}
				cases.set(cases.get() + 1);
				Ok(())
			},
		)
		.unwrap();
	assert_eq!(cases.get(), PROPERTY_CASES);
	println!(
		"HFS+ generated name triples: {} at seed {PROPERTY_SEED:#x}, up to {MAX_GENERATED_SCALARS} scalars and {MAX_NAME_UNITS} stored units",
		cases.get()
	);
}

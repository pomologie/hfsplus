use std::cmp::Ordering;

use hfsplus_core::names::{MAX_NAME_UNITS, NameCollation, NameError, normalize, normalize_utf16};

const NATIVE_NAMES: &str = include_str!("fixtures/native-normalization.tsv");
const NATIVE_COLLATION: &str = include_str!("fixtures/native-collation.tsv");
const NATIVE_CATALOG_ORDER: &str = include_str!("fixtures/native-catalog-order.txt");

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
	for (_, original, expected) in native_names().filter(|row| row.0 == OracleComparison::Agreement)
	{
		let original_str = String::from_utf16(&original).expect("fixture input UTF-16");
		assert_eq!(normalize(&original_str).unwrap().as_units(), expected);
		assert_eq!(normalize_utf16(&original).unwrap().as_units(), expected);
		assert_eq!(normalize_utf16(&expected).unwrap().as_units(), expected);
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
		assert_eq!(normalize_utf16(&original).unwrap().as_units(), original);
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

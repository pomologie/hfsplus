//! Inline attribute record coverage for H-FMT-02, H-FMT-04, and H-BUILD-03.

use hfsplus_format::{
	Error,
	attributes::{
		InlineRecord, Key, MAX_KEY_LENGTH, MAX_NAME_UNITS, NATIVE_INLINE_MAX_VALUE_BYTES,
	},
	volume::CatalogId,
};

const NATIVE_RECORDS: &[u8] = include_bytes!("fixtures/native-inline-attrs.bin");
const FIRST_RECORD_END: usize = 66;
const REGULAR_RECORDS_END: usize = 264;
const BOUNDARY_INLINE_END: usize = 4266;
const FORK_RECORD_END: usize = 4406;
const UNWRITTEN: u8 = 0xaa;
const TEST_OWNER: CatalogId = CatalogId(40);
const RESERVED_COMPRESSION_NAME: &str = "com.apple.decmpfs";

#[test]
fn matches_native_records_byte_for_byte_and_verifies_binary_order() {
	const BOUNDARY_VALUE: &[u8] = &[b's'; NATIVE_INLINE_MAX_VALUE_BYTES];
	let regular_cases = [
		(0..FIRST_RECORD_END, 18, "org.pomologie.Odd", &[0x2a][..]),
		(
			FIRST_RECORD_END..134,
			19,
			"org.pomologie.Odd",
			&[1, 0, 0xff],
		),
		(134..196, 19, "org.pomologie.a", &[0x94]),
		(
			196..REGULAR_RECORDS_END,
			19,
			"org.pomologie.even",
			&[0xbe, 0xef],
		),
	];
	let boundary_cases = [
		(REGULAR_RECORDS_END..332, 18, "org.pomologie.empty", &[][..]),
		(332..396, 18, "org.pomologie.e\u{0301}", &[2]),
		(
			396..BOUNDARY_INLINE_END,
			18,
			"org.pomologie.n3802",
			BOUNDARY_VALUE,
		),
		(FORK_RECORD_END..4468, 18, "org.pomologie.é", &[1]),
	];
	for cases in [regular_cases, boundary_cases] {
		let mut previous = None;
		for (range, file_id, name, value) in cases {
			let native = &NATIVE_RECORDS[range];
			let decoded = InlineRecord::decode(native).unwrap();
			assert_eq!(decoded.key.file_id, CatalogId(file_id));
			assert_eq!(decoded.key.start_block, 0);
			assert!(decoded.key.name.units().eq(name.encode_utf16()));
			assert_eq!(decoded.value, value);
			assert!(previous.is_none_or(|prior| prior < decoded.key));
			previous = Some(decoded.key);

			let units: Vec<_> = name.encode_utf16().collect();
			assert_eq!(InlineRecord::encoded_len(&units, value), Ok(native.len()));
			let mut encoded = vec![UNWRITTEN; native.len() + 1];
			let written =
				InlineRecord::encode(CatalogId(file_id), &units, value, &mut encoded).unwrap();
			assert_eq!(&encoded[..written], native);
			assert_eq!(encoded[written], UNWRITTEN);
		}
	}
	let fork_backed = &NATIVE_RECORDS[BOUNDARY_INLINE_END..FORK_RECORD_END];
	let key = Key::split(fork_backed).unwrap().0;
	assert!(key.name.units().eq("org.pomologie.n3803".encode_utf16()));
	assert_eq!(
		InlineRecord::decode(fork_backed),
		Err(Error::UnsupportedAttributeRecord(0x20))
	);
}

#[test]
fn keys_keep_maximum_raw_units_and_start_blocks() {
	const SURROGATE: u16 = 0xd800;
	const SECOND_BLOCK: u32 = 23;
	let name = [SURROGATE; MAX_NAME_UNITS];
	let key_len = Key::encoded_len(&name).unwrap();
	assert_eq!(key_len, MAX_KEY_LENGTH + size_of::<u16>());
	let mut first = vec![UNWRITTEN; key_len];
	let mut second = first.clone();
	assert_eq!(Key::encode(TEST_OWNER, &name, 0, &mut first), Ok(key_len));
	assert_eq!(
		Key::encode(TEST_OWNER, &name, SECOND_BLOCK, &mut second),
		Ok(key_len)
	);
	let (first, rest) = Key::split(&first).unwrap();
	let (second, _) = Key::split(&second).unwrap();
	assert!(rest.is_empty());
	assert!(second > first);
	assert_eq!(second.file_id, TEST_OWNER);
	assert_eq!(second.start_block, SECOND_BLOCK);
	assert!(second.name.units().eq(name));
}

#[test]
fn rejects_invalid_lengths_and_unsupported_records() {
	const KEY_LENGTH: usize = 0;
	const KEY_START_BLOCK: usize = 8;
	const INLINE_LENGTH: usize = 12;
	const FORK_OVERFLOW_RECORD: u32 = 0x30;
	const UNKNOWN_RECORD: u32 = 0xff;
	let original = &NATIVE_RECORDS[..FIRST_RECORD_END];
	let (_, value) = Key::split(original).unwrap();
	let key_bytes = original.len() - value.len();
	for end in 0..original.len() {
		assert!(InlineRecord::decode(&original[..end]).is_err());
	}
	let mut record = original.to_vec();
	record[KEY_LENGTH..][..2].copy_from_slice(&1u16.to_be_bytes());
	assert_eq!(InlineRecord::decode(&record), Err(Error::InvalidAttribute));
	record.copy_from_slice(original);
	record[KEY_START_BLOCK..][..4].copy_from_slice(&1u32.to_be_bytes());
	assert_eq!(InlineRecord::decode(&record), Err(Error::InvalidAttribute));
	record.copy_from_slice(original);
	let unsupported_bytes = NATIVE_INLINE_MAX_VALUE_BYTES + 1;
	record[key_bytes + INLINE_LENGTH..][..4]
		.copy_from_slice(&(unsupported_bytes as u32).to_be_bytes());
	assert_eq!(
		InlineRecord::decode(&record),
		Err(Error::UnsupportedAttributeSize(unsupported_bytes))
	);
	record.copy_from_slice(original);
	for tag in [FORK_OVERFLOW_RECORD, UNKNOWN_RECORD] {
		record[key_bytes..][..4].copy_from_slice(&tag.to_be_bytes());
		assert_eq!(
			InlineRecord::decode(&record),
			Err(Error::UnsupportedAttributeRecord(tag))
		);
	}
	record.copy_from_slice(original);
	record.extend_from_slice(&[0, 0]);
	assert_eq!(InlineRecord::decode(&record), Err(Error::InvalidAttribute));
	let compressed_name: Vec<_> = RESERVED_COMPRESSION_NAME.encode_utf16().collect();
	let key_width = Key::encoded_len(&compressed_name).unwrap();
	let mut compressed_record = vec![0; key_width];
	Key::encode(TEST_OWNER, &compressed_name, 0, &mut compressed_record).unwrap();
	compressed_record.extend_from_slice(value);
	assert_eq!(
		InlineRecord::decode(&compressed_record),
		Err(Error::UnsupportedCompressedAttribute)
	);
}

#[test]
fn invalid_inputs_leave_the_destination_intact() {
	const NAME: &[u16] = &[b'x' as u16];
	const OVERLONG_NAME: &[u16] = &[b'x' as u16; MAX_NAME_UNITS + 1];
	const OUT_OF_PROFILE: &[u8] = &[0; NATIVE_INLINE_MAX_VALUE_BYTES + 1];
	let compressed_name: Vec<_> = RESERVED_COMPRESSION_NAME.encode_utf16().collect();
	let mut unwritten = [UNWRITTEN; 6];
	for (name, value, error) in [
		(&[][..], &[][..], Error::InvalidAttribute),
		(OVERLONG_NAME, &[][..], Error::InvalidAttribute),
		(
			&compressed_name,
			&[][..],
			Error::UnsupportedCompressedAttribute,
		),
		(
			NAME,
			OUT_OF_PROFILE,
			Error::UnsupportedAttributeSize(OUT_OF_PROFILE.len()),
		),
		(NAME, &[][..], Error::Truncated),
	] {
		assert_eq!(
			InlineRecord::encode(TEST_OWNER, name, value, &mut unwritten),
			Err(error)
		);
		assert_eq!(unwritten, [UNWRITTEN; 6]);
	}
}

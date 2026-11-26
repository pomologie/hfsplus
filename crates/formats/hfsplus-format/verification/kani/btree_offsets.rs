use super::{Error, Node, offset_table_start};

// Reference HFS+ wire layout at the smallest node size accepted by the decoder.
const NODE_BYTES: usize = 512;
const DESCRIPTOR_BYTES: usize = 14;
const LEAF_KIND_OFFSET: usize = 8;
const LEAF_HEIGHT_OFFSET: usize = 9;
const RECORD_COUNT_OFFSET: usize = 10;
const LEAF_KIND: u8 = 0xff;
const LEAF_HEIGHT: u8 = 1;
const OFFSET_BYTES: usize = 2;
const FREE_SPACE_OFFSETS: usize = 1;
const MAX_RECORDS: usize = 8;
const MAX_OFFSETS: usize = MAX_RECORDS + FREE_SPACE_OFFSETS;
const MAX_NODE_SIZE_SHIFT: u32 = 6;

#[kani::proof]
#[kani::unwind(10)]
fn decode_leaf_offsets_match_bounded_wire_model() {
	let count: u8 = kani::any();
	let records = usize::from(count);
	kani::assume((1..=MAX_RECORDS).contains(&records));
	let offsets: [u16; MAX_OFFSETS] = kani::any();
	let used = &offsets[..=records];
	let mut bytes = leaf(u16::from(count));
	let table_start = NODE_BYTES - used.len() * OFFSET_BYTES;
	// Fill the fixed trailing slots. With fewer records, the unused slots are
	// unconstrained record-area bytes and the decoder sees only the active table.
	for (slot, offset) in bytes[NODE_BYTES - MAX_OFFSETS * OFFSET_BYTES..]
		.rchunks_exact_mut(OFFSET_BYTES)
		.zip(offsets)
	{
		slot.copy_from_slice(&offset.to_be_bytes());
	}

	let admitted = usize::from(used[0]) == DESCRIPTOR_BYTES
		&& used.windows(2).all(|pair| {
			let range = usize::from(pair[0])..usize::from(pair[1]);
			!range.is_empty() && range.end <= table_start && range.end & 1 == 0
		});
	match Node::decode(&bytes) {
		Ok(node) => {
			assert!(admitted);
			assert_eq!(node.records().size_hint(), (records, Some(records)));
			let ordinal: usize = kani::any();
			if ordinal < records {
				let start = usize::from(used[ordinal]);
				let end = usize::from(used[ordinal + 1]);
				let expected = &bytes[start..end];
				assert!(core::ptr::eq(node.record(ordinal).unwrap(), expected));
			} else {
				assert!(node.record(ordinal).is_none());
			}
		}
		Err(error) => {
			assert_eq!(error, Error::InvalidNode);
			assert!(!admitted);
		}
	}

	let first_two_offsets_valid = records == 2
		&& usize::from(offsets[0]) == DESCRIPTOR_BYTES
		&& usize::from(offsets[1]) == DESCRIPTOR_BYTES + OFFSET_BYTES;
	kani::cover!(
		first_two_offsets_valid && usize::from(offsets[2]) == DESCRIPTOR_BYTES,
		"decreasing offsets"
	);
	kani::cover!(
		first_two_offsets_valid && offsets[2] == offsets[1],
		"duplicate offsets"
	);
	kani::cover!(
		first_two_offsets_valid && usize::from(offsets[2]) == table_start + OFFSET_BYTES,
		"offset past the table start"
	);
	kani::cover!(
		admitted && records == MAX_RECORDS && usize::from(used[records]) == table_start,
		"eight records reaching the table boundary"
	);
}

#[kani::proof]
#[kani::unwind(2)]
fn table_admission_matches_full_width_wire_counts() {
	let size_shift: u32 = kani::any();
	kani::assume(size_shift <= MAX_NODE_SIZE_SHIFT);
	let node_bytes = NODE_BYTES << size_shift;
	let count: u16 = kani::any();
	let table_width = (i64::from(count) + FREE_SPACE_OFFSETS as i64) * OFFSET_BYTES as i64;
	let start = node_bytes as i64 - table_width;
	let expected = if start < DESCRIPTOR_BYTES as i64 {
		Err(Error::InvalidNode)
	} else {
		Ok(usize::try_from(start).unwrap())
	};
	assert_eq!(offset_table_start(node_bytes, usize::from(count)), expected);
	if node_bytes == NODE_BYTES && expected.is_err() {
		let mut raw_leaf = leaf(count);
		raw_leaf[NODE_BYTES - OFFSET_BYTES..]
			.copy_from_slice(&(DESCRIPTOR_BYTES as u16).to_be_bytes());
		assert!(matches!(Node::decode(&raw_leaf), Err(Error::InvalidNode)));
	}
	kani::cover!(
		node_bytes == NODE_BYTES && start + OFFSET_BYTES as i64 == DESCRIPTOR_BYTES as i64,
		"first count beyond minimum node table capacity"
	);
	kani::cover!(count == u16::MAX, "largest representable record count");
}

fn leaf(count: u16) -> [u8; NODE_BYTES] {
	let mut bytes = [0; NODE_BYTES];
	bytes[LEAF_KIND_OFFSET] = LEAF_KIND;
	bytes[LEAF_HEIGHT_OFFSET] = LEAF_HEIGHT;
	bytes[RECORD_COUNT_OFFSET..][..OFFSET_BYTES].copy_from_slice(&count.to_be_bytes());
	bytes
}

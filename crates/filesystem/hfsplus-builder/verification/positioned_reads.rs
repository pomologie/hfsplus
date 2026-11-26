use hfsplus_core::{ByteLen, ByteOffset, ByteRange, RangeError, ReadAt};

use crate::{Limits, PlanBuilder, RangeKind, ReadError};

const MAX_PLACEMENT_BYTES: usize = 2;
const METADATA: &[u8] = b"M!";
const ZERO: &[u8] = &[0; MAX_PLACEMENT_BYTES];
const DIRECT_SOURCE: &[u8] = b"?D+";
const DIRECT_START: usize = 1;
const SHARED_SOURCE: &[u8] = b"?XxYy?";
const SHARED_START: usize = 1;
const SHARED_BYTES: usize = 4;
const SHARED_SECOND_START: usize = 2;
const SENTINEL: u8 = 0xff;

#[derive(Clone, Copy, Debug)]
enum Segment {
	Metadata,
	Zero,
	Direct,
	SharedFirst,
	SharedSecond,
}

const SEGMENTS: [Segment; 5] = [
	Segment::Metadata,
	Segment::Zero,
	Segment::Direct,
	Segment::SharedFirst,
	Segment::SharedSecond,
];

impl Segment {
	const fn reference(self) -> &'static [u8] {
		match self {
			Self::Metadata => b"M!",
			Self::Zero => ZERO,
			Self::Direct => b"D+",
			Self::SharedFirst => b"Xx",
			Self::SharedSecond => b"Yy",
		}
	}

	const fn kind(self) -> RangeKind {
		match self {
			Self::Metadata => RangeKind::Metadata,
			Self::Zero => RangeKind::Zero,
			Self::Direct | Self::SharedFirst | Self::SharedSecond => RangeKind::Source,
		}
	}
}

fn check_layout(order: &[Segment; SEGMENTS.len()], lengths: [usize; SEGMENTS.len()]) {
	let image_bytes = lengths.iter().sum::<usize>();
	let mut builder = PlanBuilder::new(image_bytes.into(), Limits::default());
	let window = ByteRange::new(
		ByteOffset(SHARED_START as u64),
		ByteLen::from(SHARED_BYTES),
		SHARED_SOURCE.len().into(),
	)
	.unwrap();
	let source = builder.register_source(SHARED_SOURCE, window).unwrap();
	let mut reference = Vec::with_capacity(image_bytes);
	let mut end = 0;
	for (segment, bytes) in order.iter().copied().zip(lengths) {
		let start = ByteOffset(end as u64);
		let len = ByteLen::from(bytes);
		match segment {
			Segment::Metadata => builder.push_metadata(start, Box::from(&METADATA[..bytes])),
			Segment::Zero => builder.push_zero(start, len),
			Segment::Direct => {
				builder.push_source(start, DIRECT_SOURCE, ByteOffset(DIRECT_START as u64), len)
			}
			Segment::SharedFirst => builder.push_source_range(start, source, ByteOffset(0), len),
			Segment::SharedSecond => builder.push_source_range(
				start,
				source,
				ByteOffset(SHARED_SECOND_START as u64),
				len,
			),
		}
		.unwrap();
		reference.extend_from_slice(&segment.reference()[..bytes]);
		end += bytes;
	}
	let plan = builder.seal().unwrap();
	assert_eq!(plan.ranges().len(), order.len());
	let mut end = 0;
	for ((range, kind), (segment, bytes)) in plan.ranges().zip(order.iter().zip(lengths)) {
		assert_eq!(range.start(), ByteOffset(end));
		assert_eq!(range.len(), ByteLen::from(bytes));
		assert_eq!(kind, segment.kind());
		end += bytes as u64;
	}
	assert_eq!(end, image_bytes as u64);

	let mut sequential = Vec::new();
	plan.write_to(&mut sequential).unwrap();
	assert_eq!(
		sequential, reference,
		"layout {order:?} lengths {lengths:?}"
	);
	let mut observed = [SENTINEL; SEGMENTS.len() * MAX_PLACEMENT_BYTES];
	for start in 0..=image_bytes {
		for end in start..=image_bytes {
			observed.fill(SENTINEL);
			plan.read_exact_at(ByteOffset(start as u64), &mut observed[..end - start])
				.unwrap();
			assert_eq!(
				&observed[..end - start],
				&reference[start..end],
				"layout {order:?} lengths {lengths:?}, request {start}..{end}",
			);
			assert!(observed[end - start..].iter().all(|&byte| byte == SENTINEL));
		}
	}
	for (offset, expected) in [
		(ByteOffset(image_bytes as u64), RangeError::OutsideSource),
		(ByteOffset(u64::MAX), RangeError::Overflow),
	] {
		let mut unchanged = [SENTINEL];
		assert!(matches!(
			plan.read_exact_at(offset, &mut unchanged),
			Err(ReadError::Bounds(error)) if error == expected
		));
		assert_eq!(unchanged, [SENTINEL]);
	}
}

fn enumerate_orders(order: &mut [Segment; SEGMENTS.len()], first: usize, checks: &mut usize) {
	if first == order.len() {
		for mask in 0..(1 << order.len()) {
			let lengths = std::array::from_fn(|index| 1 + ((mask >> index) & 1));
			check_layout(order, lengths);
			*checks += 1;
		}
		return;
	}
	for index in first..order.len() {
		order.swap(first, index);
		enumerate_orders(order, first + 1, checks);
		order.swap(first, index);
	}
}

#[test]
fn every_small_layout_matches_its_independent_materialized_byte_model() {
	let mut checks = 0;
	let mut order = SEGMENTS;
	enumerate_orders(&mut order, 0, &mut checks);
	let permutations: usize = (1..=SEGMENTS.len()).product();
	assert_eq!(
		checks,
		permutations * MAX_PLACEMENT_BYTES.pow(SEGMENTS.len() as u32)
	);
}

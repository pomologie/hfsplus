use std::{
	io::{self, Read, Write},
	thread,
};

use hfsplus_builder::{
	Limits, PlanBuilder, PlanError, RangeKind, ReadError, SealedPlan, WriteError,
};
use hfsplus_core::{ByteLen, ByteOffset, RangeError, ReadAt, WindowError};

const REFERENCE: &[u8] = b"meta\0\0filedata";

fn plan() -> SealedPlan<&'static [u8]> {
	let mut plan = PlanBuilder::new(REFERENCE.len().into(), Limits::default());
	plan.push_metadata(ByteOffset(0), Box::from(*b"meta"))
		.unwrap();
	plan.push_zero(ByteOffset(4), ByteLen(2)).unwrap();
	plan.push_source(
		ByteOffset(6),
		b"prefix-file".as_slice(),
		ByteOffset(7),
		ByteLen(4),
	)
	.unwrap();
	plan.push_source(
		ByteOffset(10),
		b"data-suffix".as_slice(),
		ByteOffset(0),
		ByteLen(4),
	)
	.unwrap();
	plan.seal().unwrap()
}

#[test]
fn every_partial_request_matches_materialized_bytes() {
	let plan = plan();
	assert_eq!(
		plan.ranges().map(|(_, kind)| kind).collect::<Vec<_>>(),
		[
			RangeKind::Metadata,
			RangeKind::Zero,
			RangeKind::Source,
			RangeKind::Source
		]
	);
	thread::scope(|scope| {
		for start in 0..=REFERENCE.len() {
			let plan = &plan;
			scope.spawn(move || {
				for end in start..=REFERENCE.len() {
					let mut bytes = vec![u8::MAX; end - start];
					plan.read_exact_at(ByteOffset(start as u64), &mut bytes)
						.unwrap();
					assert_eq!(bytes, REFERENCE[start..end]);
				}
			});
		}
	});
	assert!(matches!(
		plan.read_exact_at(ByteOffset(REFERENCE.len() as u64), &mut [0]),
		Err(ReadError::Bounds(RangeError::OutsideSource))
	));
}

#[test]
fn writes_to_a_nonseekable_short_write_sink_and_exposes_a_reader() {
	#[derive(Default)]
	struct ShortWriter(Vec<u8>);
	impl Write for ShortWriter {
		fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
			let count = bytes.len().min(2);
			self.0.extend_from_slice(&bytes[..count]);
			Ok(count)
		}
		fn flush(&mut self) -> io::Result<()> {
			Ok(())
		}
	}
	let plan = plan();
	let mut output = ShortWriter::default();
	plan.write_to(&mut output).unwrap();
	assert_eq!(output.0, REFERENCE);
	let mut sequential = Vec::new();
	plan.reader().read_to_end(&mut sequential).unwrap();
	assert_eq!(sequential, REFERENCE);
}

#[test]
fn rejects_gaps_overlaps_bounds_and_budgets_without_advancing_the_prefix() {
	let account = Limits {
		max_ranges: 2,
		max_metadata_bytes: ByteLen(2),
	};
	let mut draft = PlanBuilder::<&[u8]>::new(ByteLen(4), account);
	assert!(matches!(
		draft.push_zero(ByteOffset(1), ByteLen(1)),
		Err(PlanError::OutOfOrder { .. })
	));
	assert!(matches!(
		draft.push_zero(ByteOffset(0), ByteLen(0)),
		Err(PlanError::EmptyRange)
	));
	assert!(matches!(
		draft.push_metadata(ByteOffset(0), Box::from(*b"abc")),
		Err(PlanError::MetadataLimit)
	));
	assert!(matches!(
		draft.push_source(ByteOffset(0), b"x", ByteOffset(u64::MAX), ByteLen(1)),
		Err(PlanError::SourceRange(RangeError::Overflow))
	));
	draft
		.push_metadata(ByteOffset(0), Box::from(*b"ab"))
		.unwrap();
	assert!(matches!(
		draft.push_zero(ByteOffset(1), ByteLen(1)),
		Err(PlanError::OutOfOrder { .. })
	));
	assert!(matches!(
		draft.push_zero(ByteOffset(2), ByteLen(3)),
		Err(PlanError::ImageRange(RangeError::OutsideSource))
	));
	draft.push_zero(ByteOffset(2), ByteLen(1)).unwrap();
	assert!(matches!(
		draft.push_zero(ByteOffset(3), ByteLen(1)),
		Err(PlanError::RangeLimit)
	));
	assert!(matches!(
		draft.seal(),
		Err(PlanError::Incomplete { end: ByteOffset(3) })
	));
}

#[test]
fn preserves_source_faults_sink_faults_and_cancellation() {
	#[derive(Debug)]
	struct Fault {
		kind: io::ErrorKind,
	}
	impl ReadAt for Fault {
		type Error = io::Error;
		fn len(&self) -> ByteLen {
			ByteLen(1)
		}
		fn read_exact_at(&self, _: ByteOffset, _: &mut [u8]) -> Result<(), Self::Error> {
			Err(self.kind.into())
		}
	}
	for kind in [io::ErrorKind::UnexpectedEof, io::ErrorKind::InvalidData] {
		let mut draft = PlanBuilder::new(ByteLen(2), Limits::default());
		draft.push_zero(ByteOffset(0), ByteLen(1)).unwrap();
		draft
			.push_source(ByteOffset(1), Fault { kind }, ByteOffset(0), ByteLen(1))
			.unwrap();
		let mut bytes = Vec::new();
		let result = draft.seal().unwrap().write_to(&mut bytes);
		assert!(
			matches!(result, Err(WriteError::Read(ReadError::Source { offset: ByteOffset(1), error: WindowError::Source(error) })) if error.kind() == kind)
		);
		assert!(bytes.is_empty());
	}
	let plan = plan();
	let result = plan.write_to(&mut [0; 1][..]);
	assert!(
		matches!(result, Err(WriteError::Sink(error)) if error.kind() == io::ErrorKind::WriteZero)
	);
	let mut calls = 0;
	let mut output = Vec::new();
	assert!(matches!(
		plan.write_with_cancel(&mut output, || {
			calls += 1;
			calls > 1
		}),
		Err(WriteError::Cancelled)
	));
	assert!(output.is_empty());
}

#[test]
fn large_zero_range_requires_only_the_requested_window() {
	const IMAGE_BYTES: ByteLen = ByteLen(5 * 1024 * 1024 * 1024);
	let mut draft = PlanBuilder::<&[u8]>::new(
		IMAGE_BYTES,
		Limits {
			max_ranges: 1,
			max_metadata_bytes: ByteLen(0),
		},
	);
	draft.push_zero(ByteOffset(0), IMAGE_BYTES).unwrap();
	let plan = draft.seal().unwrap();
	let mut bytes = [u8::MAX; 16];
	plan.read_exact_at(ByteOffset(IMAGE_BYTES.0 - bytes.len() as u64), &mut bytes)
		.unwrap();
	assert_eq!(bytes, [0; 16]);
}

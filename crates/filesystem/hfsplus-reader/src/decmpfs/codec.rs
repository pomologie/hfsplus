use alloc::{collections::TryReserveError, vec::Vec};
use core::fmt;

use miniz_oxide::inflate::{
	TINFLStatus,
	core::{
		DecompressorOxide, decompress,
		inflate_flags::{TINFL_FLAG_PARSE_ZLIB_HEADER, TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF},
	},
};

const STORED_MARKER: u8 = 0xff;

/// A single decoded frame violated its zlib or exact-length contract.
#[derive(Debug)]
#[non_exhaustive]
pub enum FrameError {
	/// The input is not a complete zlib stream or an exact stored frame.
	InvalidStream,
	/// The zlib checksum does not match the decoded block.
	Checksum,
	/// Decoding did not produce exactly the advertised byte count.
	Length,
	/// Bytes follow an otherwise valid zlib stream.
	TrailingBytes,
	/// Fixed decoder state could not be reserved.
	Allocation(TryReserveError),
}

impl fmt::Display for FrameError {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::InvalidStream => formatter.write_str("invalid zlib or stored frame"),
			Self::Checksum => formatter.write_str("zlib checksum mismatch"),
			Self::Length => formatter.write_str("decoded length differs from the block length"),
			Self::TrailingBytes => formatter.write_str("bytes follow the zlib stream"),
			Self::Allocation(error) => error.fmt(formatter),
		}
	}
}

impl core::error::Error for FrameError {
	fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
		match self {
			Self::Allocation(error) => Some(error),
			_ => None,
		}
	}
}

#[derive(Default)]
pub(super) struct Decoder {
	// Heap-backed fixed zlib state reused across blocks in one read.
	state: Vec<DecompressorOxide>,
}

impl Decoder {
	pub(super) fn decode(&mut self, input: &[u8], output: &mut [u8]) -> Result<(), FrameError> {
		let (&marker, payload) = input.split_first().ok_or(FrameError::InvalidStream)?;
		if marker == STORED_MARKER {
			if payload.len() != output.len() {
				return Err(FrameError::Length);
			}
			output.copy_from_slice(payload);
			return Ok(());
		}
		if self.state.is_empty() {
			self.state
				.try_reserve_exact(1)
				.map_err(FrameError::Allocation)?;
			self.state.push(DecompressorOxide::new());
		}
		let state = &mut self.state[0];
		state.init();
		let flags = TINFL_FLAG_PARSE_ZLIB_HEADER | TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF;
		let (status, consumed, written) = decompress(state, input, output, 0, flags);
		match status {
			TINFLStatus::Done if consumed != input.len() => Err(FrameError::TrailingBytes),
			TINFLStatus::Done if written == output.len() => Ok(()),
			TINFLStatus::Done | TINFLStatus::HasMoreOutput => Err(FrameError::Length),
			TINFLStatus::Adler32Mismatch => Err(FrameError::Checksum),
			_ => Err(FrameError::InvalidStream),
		}
	}
}

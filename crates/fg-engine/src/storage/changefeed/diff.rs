//! Before representation = after prefix + stored middle + after suffix.

use super::{CodecBudget, Decoder, checked_payload, invalid};
use crate::gql_status::FerrosGrynnResult;

/// Fixed checked fields: before length, prefix, suffix and middle length.
const HEADER_BYTES: usize = 32;

/// Encodes an exact inverse diff without consulting a current primary record.
pub(super) fn encode(
	before: &[u8],
	after: &[u8],
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<Vec<u8>> {
	checked_payload(before.len() as u64)?;
	checked_payload(after.len() as u64)?;
	let prefix = before.iter().zip(after).take_while(|(left, right)| left == right).count();
	let suffix = before[prefix..]
		.iter()
		.rev()
		.zip(after[prefix..].iter().rev())
		.take_while(|(left, right)| left == right)
		.count();
	let middle = &before[prefix..before.len() - suffix];
	let bytes = HEADER_BYTES
		.checked_add(middle.len())
		.ok_or_else(|| invalid("changefeed diff length overflows"))?;
	checked_payload(bytes as u64)?;
	budget.admit(bytes)?;
	let mut output = Vec::new();
	output
		.try_reserve_exact(bytes)
		.map_err(|_error| invalid("changefeed diff allocation failed"))?;
	for value in [before.len(), prefix, suffix, middle.len()] {
		output.extend_from_slice(&(value as u64).to_le_bytes());
	}
	output.extend_from_slice(middle);
	Ok(output)
}

/// Restores complete before bytes only after every patch reference is checked.
pub(super) fn decode(
	diff: &[u8],
	after: &[u8],
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<Vec<u8>> {
	checked_payload(diff.len() as u64)?;
	checked_payload(after.len() as u64)?;
	let mut decoder = Decoder::new(diff);
	let before = checked_payload(decoder.u64()?)?;
	let prefix = checked_payload(decoder.u64()?)?;
	let suffix = checked_payload(decoder.u64()?)?;
	let middle = checked_payload(decoder.u64()?)?;
	let copied =
		prefix.checked_add(suffix).ok_or_else(|| invalid("changefeed diff references overflow"))?;
	if copied > after.len() || copied.checked_add(middle) != Some(before) {
		return Err(invalid("changefeed diff references or output length are invalid"));
	}
	let middle = decoder.take(middle)?;
	decoder.finish()?;
	budget.admit(before)?;
	let mut output = Vec::new();
	output
		.try_reserve_exact(before)
		.map_err(|_error| invalid("changefeed before image allocation failed"))?;
	output.extend_from_slice(&after[..prefix]);
	output.extend_from_slice(middle);
	output.extend_from_slice(&after[after.len() - suffix..]);
	Ok(output)
}

#[cfg(test)]
/// Regression tests for diff behavior.
mod tests {
	use super::{HEADER_BYTES, decode, encode};
	use crate::storage::changefeed::CodecBudget;

	/// Verifies changefeed inverse diff roundtrips insert delete restore and
	/// common edges.
	#[test]
	fn changefeed_inverse_diff_roundtrips_insert_delete_restore_and_common_edges() {
		use crate::test_assertions::AssertResult as _;

		for (before, after) in [
			(b"".as_slice(), b"".as_slice()),
			(b"abc", b""),
			(b"", b"abc"),
			(b"abc", b"abc"),
			(b"prefix-old-suffix", b"prefix-new-suffix"),
			(b"aaa", b"aa"),
			(b"abc", b"xyz"),
		] {
			let mut budget = CodecBudget::new(1024, 0).assert_ok();
			let diff = encode(before, after, &mut budget).assert_ok();
			assert_eq!(decode(&diff, after, &mut budget).assert_ok(), before);
		}
		let mut budget = CodecBudget::new(1024, 0).assert_ok();
		let diff = encode(b"abc", b"abc", &mut budget).assert_ok();
		let mut golden = Vec::new();
		for field in [3_u64, 3, 0, 0] {
			golden.extend_from_slice(&field.to_le_bytes());
		}
		assert_eq!(diff, golden);
		assert_eq!(diff.len(), HEADER_BYTES);
	}

	/// Verifies changefeed inverse diff rejects overlap overflow truncation and
	/// budget.
	#[test]
	fn changefeed_inverse_diff_rejects_overlap_overflow_truncation_and_budget() {
		use crate::test_assertions::AssertResult as _;

		let mut budget = CodecBudget::new(1024, 0).assert_ok();
		let valid = encode(b"prefix-old-suffix", b"prefix-new-suffix", &mut budget).assert_ok();
		for count in 0..valid.len() {
			assert!(
				decode(
					&valid[..count],
					b"prefix-new-suffix",
					&mut CodecBudget::new(1024, 0).assert_ok()
				)
				.is_err()
			);
		}
		for field in 0..4 {
			let mut invalid = valid.clone();
			invalid[field * 8..field * 8 + 8].copy_from_slice(&u64::MAX.to_le_bytes());
			assert!(
				decode(&invalid, b"prefix-new-suffix", &mut CodecBudget::new(1024, 0).assert_ok())
					.is_err()
			);
		}
		assert!(
			decode(&valid, b"prefix-new-suffix", &mut CodecBudget::new(1, 0).assert_ok()).is_err()
		);
		assert!(encode(b"a", b"b", &mut CodecBudget::new(1, 0).assert_ok()).is_err());
	}
}

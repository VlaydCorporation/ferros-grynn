//! Checked logical ORDER and exact output-window fields in the FGLD body.

use super::{CodecBudget, Decoder, Work, append, decode_expr, encode_expr, invalid, values};
use crate::{
	gql_status::FerrosGrynnResult,
	live::{LiveExpr, LiveNullsOrder, LiveOrder, LiveOrderDirection, LiveOrderKey, LiveQuery},
	values::storage::EncodeContext,
};

/// Restored logical clauses, validated before registry admission.
pub(super) struct Window {
	/// Ordered deterministic keys.
	pub(super) order: Vec<LiveOrder>,
	/// Optional exact row count expression.
	pub(super) limit: Option<LiveExpr>,
	/// Optional exact row offset expression.
	pub(super) start_offset: Option<LiveExpr>,
}

/// Writes checked key count, key header/payloads and optional bound
/// expressions.
pub(super) fn encode(
	output: &mut Vec<u8>,
	query: &LiveQuery,
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work<'_>,
	depth: usize,
) -> FerrosGrynnResult<()> {
	work.take(depth, query.order().len())?;
	values::write_count(output, query.order().len(), budget)?;
	for order in query.order() {
		let flags = u8::from(order.direction() == LiveOrderDirection::Descending)
			| (u8::from(order.nulls() == LiveNullsOrder::First) << 1)
			| (u8::from(order.collation()) << 2)
			| (u8::from(order.numeric()) << 3);
		match order.key() {
			LiveOrderKey::Expression(value) => {
				append(output, &[0, flags, 0, 0], budget)?;
				encode_expr(output, value, context, budget, work, depth + 1)?;
			}
			LiveOrderKey::Name(name) => {
				if name.is_empty() || name.as_str().contains('\0') {
					return Err(invalid("LIVE ORDER name is empty or contains NUL"));
				}
				append(output, &[1, flags, 0, 0], budget)?;
				values::append_text(output, name, budget)?;
			}
		}
	}
	for value in [query.limit(), query.start_offset()] {
		append(output, &[u8::from(value.is_some())], budget)?;
		if let Some(value) = value {
			encode_expr(output, value, context, budget, work, depth + 1)?;
		}
	}
	Ok(())
}

/// Bounds count, flags, reserved bytes and recursion before retaining data.
pub(super) fn decode(
	decoder: &mut Decoder<'_>,
	context: &EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work<'_>,
	depth: usize,
) -> FerrosGrynnResult<Window> {
	// The shortest expression is an unbounded Range: two bytes after the
	// header.
	let count = values::count(decoder, 6)?;
	work.take(depth, count)?;
	budget.admit(
		count
			.checked_mul(std::mem::size_of::<LiveOrder>())
			.ok_or_else(|| invalid("LIVE ORDER size overflows"))?,
	)?;
	let mut order = Vec::new();
	order.try_reserve_exact(count).map_err(|_error| invalid("LIVE ORDER allocation failed"))?;
	for _ in 0..count {
		let tag = decoder.u8()?;
		let flags = decoder.u8()?;
		if flags & !15 != 0 || decoder.take(2)? != [0, 0] {
			return Err(invalid("LIVE ORDER flags or reserved bytes are invalid"));
		}
		let key = match tag {
			0 => LiveOrder::new(decode_expr(decoder, context, budget, work, depth + 1)?),
			1 => {
				let name = values::text(decoder, budget)?;
				if name.is_empty() || name.as_str().contains('\0') {
					return Err(invalid("LIVE ORDER name is empty or contains NUL"));
				}
				LiveOrder::by_name(name)?
			}
			_ => return Err(invalid("LIVE ORDER key tag is unknown")),
		};
		order.push(
			key.with_direction(if flags & 1 == 0 {
				LiveOrderDirection::Ascending
			} else {
				LiveOrderDirection::Descending
			})
			.with_nulls(if flags & 2 == 0 {
				LiveNullsOrder::Last
			} else {
				LiveNullsOrder::First
			})
			.with_collation(flags & 4 != 0)
			.with_numeric(flags & 8 != 0),
		);
	}
	let mut bound = || match decoder.u8()? {
		0 => Ok(None),
		1 => decode_expr(decoder, context, budget, work, depth + 1).map(Some),
		_ => Err(invalid("LIVE window expression flag is invalid")),
	};
	let limit = bound()?;
	let start_offset = bound()?;
	Ok(Window {
		order,
		limit,
		start_offset,
	})
}

#[cfg(test)]
/// Regression tests for window behavior.
mod tests {
	use super::*;
	use crate::{
		datastore::ClassId,
		live::LiveSource,
		storage::live_descriptor::{self, LiveDescriptor},
	};

	/// Golden key headers and all flag combinations remain canonical and
	/// bounded.
	#[test]
	fn live_window_descriptor_checks_flags_counts_and_names() {
		use crate::test_assertions::AssertResult as _;

		let base = LiveQuery::new(LiveSource::Class(ClassId::from_storage(1).assert_ok()));
		for flags in 0_u8..16 {
			let key = LiveOrder::by_name("k")
				.assert_ok()
				.with_direction(if flags & 1 == 0 {
					LiveOrderDirection::Ascending
				} else {
					LiveOrderDirection::Descending
				})
				.with_nulls(if flags & 2 == 0 {
					LiveNullsOrder::Last
				} else {
					LiveNullsOrder::First
				})
				.with_collation(flags & 4 != 0)
				.with_numeric(flags & 8 != 0);
			let query = base.clone().with_order(vec![key]);
			let mut bytes = Vec::new();
			let mut context = EncodeContext::default();
			encode(
				&mut bytes,
				&query,
				&mut context,
				&mut CodecBudget::new(1 << 20, 0).assert_ok(),
				&mut Work {
					observe: None,
					remaining: 1000,
				},
				0,
			)
			.assert_ok();
			let mut golden = 1_u32.to_le_bytes().to_vec();
			golden.extend_from_slice(&[1, flags, 0, 0]);
			golden.extend_from_slice(&1_u64.to_le_bytes());
			golden.extend_from_slice(&[b'k', 0, 0]);
			assert_eq!(bytes, golden);
			let decoded = decode(
				&mut Decoder::new(&bytes),
				&context,
				&mut CodecBudget::new(1 << 20, 0).assert_ok(),
				&mut Work {
					observe: None,
					remaining: 1000,
				},
				0,
			)
			.assert_ok();
			assert_eq!(decoded.order, query.order());
			assert!(decoded.limit.is_none() && decoded.start_offset.is_none());
			let descriptor = LiveDescriptor::Query(query);
			let frame = live_descriptor::encode(&descriptor, 1 << 20).assert_ok();
			assert_eq!(live_descriptor::decode(&frame, 1 << 20).assert_ok(), descriptor);
			for length in 0..frame.len() {
				assert!(live_descriptor::decode(&frame[..length], 1 << 20).is_err());
			}
			let bad_inputs = [
				(0, 255),
				(4, 2),
				(5, 16),
				(6, 1),
				(7, 1),
				(8, 0),
				(15, 255),
				(16, 0),
				(17, 2),
				(18, 2),
			];
			for (offset, byte) in bad_inputs {
				let mut bad = bytes.clone();
				bad[offset] = byte;
				assert!(
					decode(
						&mut Decoder::new(&bad),
						&context,
						&mut CodecBudget::new(1 << 20, 0).assert_ok(),
						&mut Work {
							observe: None,
							remaining: 1000
						},
						0
					)
					.is_err(),
					"flags {flags}, offset {offset}"
				);
			}
			assert!(
				decode(
					&mut Decoder::new(&bytes),
					&context,
					&mut CodecBudget::new(1 << 20, 0).assert_ok(),
					&mut Work {
						observe: None,
						remaining: 0
					},
					0
				)
				.is_err()
			);
			assert!(
				decode(
					&mut Decoder::new(&bytes),
					&context,
					&mut CodecBudget::new(8, 0).assert_ok(),
					&mut Work {
						observe: None,
						remaining: 1000
					},
					0
				)
				.is_err()
			);
		}
		let mut value = LiveExpr::Range {
			start: None,
			end: None,
			inclusive_end: false,
		};
		for _ in 0..65 {
			value = LiveExpr::Not(Box::new(value));
		}
		assert!(
			live_descriptor::encode(&LiveDescriptor::Query(base.with_limit(value)), 1 << 20)
				.is_err()
		);
	}
}

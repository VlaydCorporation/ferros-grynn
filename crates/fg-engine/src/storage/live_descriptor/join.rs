//! Canonical ordered JOIN recipes; execution and source resolution stay in GQL.

use super::{Work, decode_expr, decode_source, encode_expr, encode_source, encode_text};
use crate::{
	gql_status::FerrosGrynnResult,
	live::{LiveJoin, LiveJoinKind, LiveQuery},
	storage::history_codec::{CodecBudget, Decoder, append, invalid, values},
	values::storage::EncodeContext,
};

/// Writes one admitted nonempty declaration list in logical source order.
pub(super) fn encode(
	output: &mut Vec<u8>,
	joins: &[LiveJoin],
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work<'_>,
	depth: usize,
) -> FerrosGrynnResult<()> {
	if joins.is_empty() {
		return Err(invalid("LIVE JOIN tail is empty"));
	}
	work.take(depth, joins.len())?;
	values::write_count(output, joins.len(), budget)?;
	for join in joins {
		LiveQuery::validate_alias(join.alias().as_str())
			.map_err(|error| invalid("LIVE JOIN alias is malformed").with_cause(error))?;
		if matches!(join.kind(), LiveJoinKind::Cross) == join.predicate().is_some() {
			return Err(invalid("LIVE JOIN predicate disagrees with its kind"));
		}
		let kind = match join.kind() {
			LiveJoinKind::Inner => 0,
			LiveJoinKind::Left => 1,
			LiveJoinKind::Cross => 2,
		};
		append(output, &[kind, 0, 0, 0], budget)?;
		work.take(depth, 1)?;
		encode_text(output, join.alias(), budget, work)?;
		encode_source(output, join.source(), context, budget, work, depth)?;
		append(output, &[u8::from(join.predicate().is_some())], budget)?;
		if let Some(predicate) = join.predicate() {
			encode_expr(output, predicate, context, budget, work, depth + 1)?;
		}
	}
	Ok(())
}

/// Rejects lengths, kind, reserved bytes and predicate shape before acceptance.
pub(super) fn decode(
	decoder: &mut Decoder<'_>,
	context: &EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work<'_>,
	depth: usize,
) -> FerrosGrynnResult<Vec<LiveJoin>> {
	let count = values::count(decoder, 17)?;
	if count == 0 {
		return Err(invalid("LIVE JOIN tail is empty"));
	}
	work.take(depth, count)?;
	budget.admit(
		count
			.checked_mul(std::mem::size_of::<LiveJoin>())
			.ok_or_else(|| invalid("LIVE JOIN capacity overflows"))?,
	)?;
	let mut joins = Vec::new();
	joins
		.try_reserve_exact(count)
		.map_err(|error| invalid("LIVE JOIN allocation failed").with_technical_source(error))?;
	for _ in 0..count {
		let kind = match decoder.u8()? {
			0 => LiveJoinKind::Inner,
			1 => LiveJoinKind::Left,
			2 => LiveJoinKind::Cross,
			_ => return Err(invalid("LIVE JOIN kind is unknown")),
		};
		decoder.zero(3)?;
		work.take(depth, 1)?;
		let alias = values::text(decoder, budget)?;
		let source = decode_source(decoder, context, budget, work, depth)?;
		let predicate = match decoder.u8()? {
			0 => None,
			1 => Some(decode_expr(decoder, context, budget, work, depth + 1)?),
			_ => return Err(invalid("LIVE JOIN predicate flag is invalid")),
		};
		joins
			.push(LiveJoin::new(source, alias, kind, predicate).map_err(|error| {
				invalid("LIVE JOIN declaration is malformed").with_cause(error)
			})?);
	}
	Ok(joins)
}

#[cfg(test)]
/// Regression tests for join behavior.
mod tests {
	use super::*;
	use crate::{
		datastore::ClassId,
		live::{LiveBindingId, LiveExpr, LiveSource},
		storage::live_descriptor::LiveDescriptor,
		values::Uuid,
	};

	/// Default bytes remain stable while JOIN shape, order and limits are
	/// checked.
	#[test]
	fn live_join_descriptor_tail_and_dependencies() {
		use crate::test_assertions::AssertResult as _;

		let class = ClassId::from_storage(1).assert_ok();
		let base = LiveQuery::new(LiveSource::Class(class));
		let join = LiveJoin::new(
			LiveSource::Class(ClassId::from_storage(2).assert_ok()),
			"r",
			LiveJoinKind::Cross,
			None,
		)
		.assert_ok();
		assert_eq!(join.alias().as_str(), "r");
		assert_eq!(join.kind(), LiveJoinKind::Cross);
		assert!(join.predicate().is_none());
		let descriptor = LiveDescriptor::Query(base.clone().with_joins(vec![join]));
		let frame = super::super::encode(&descriptor, 1 << 20).assert_ok();
		assert_eq!(frame.len(), 90);
		assert_eq!(frame[66], 32);
		assert_eq!(&frame[67..71], &1_u32.to_le_bytes());
		assert_eq!(&frame[71..75], &[2, 0, 0, 0]);
		assert_eq!(&frame[75..83], &1_u64.to_le_bytes());
		assert_eq!(&frame[83..90], &[b'r', 0, 2, 0, 0, 0, 0]);
		assert_eq!(super::super::decode(&frame, 1 << 20).assert_ok(), descriptor);
		assert_eq!(
			super::super::encode(&LiveDescriptor::Query(base.clone()), 1 << 20).assert_ok().len(),
			67
		);
		for (offset, byte) in [(66, 64), (71, 3), (72, 1), (83, 0), (83, 255), (89, 2)] {
			let mut bad = frame.clone();
			bad[offset] = byte;
			assert!(super::super::decode(&bad, 1 << 20).is_err(), "offset {offset}");
		}
		for count in [0_u32, u32::MAX] {
			let mut bad = frame.clone();
			bad[67..71].copy_from_slice(&count.to_le_bytes());
			assert!(super::super::decode(&bad, 1 << 20).is_err());
		}
		for length in 0..frame.len() {
			assert!(super::super::decode(&frame[..length], 1 << 20).is_err());
		}
		for kind in [LiveJoinKind::Inner, LiveJoinKind::Left, LiveJoinKind::Cross] {
			for present in [false, true] {
				let result = LiveJoin::new(
					LiveSource::Class(class),
					"r",
					kind,
					present.then(|| LiveExpr::Literal(true.into())),
				);
				assert_eq!(result.is_ok(), matches!(kind, LiveJoinKind::Cross) != present);
			}
		}
		for alias in ["", "bad\0alias"] {
			assert!(
				LiveJoin::new(LiveSource::Class(class), alias, LiveJoinKind::Cross, None).is_err()
			);
		}
		let scope = LiveBindingId::from_uuid(Uuid::now_v7()).assert_ok();
		let predicate = LiveBindingId::from_uuid(Uuid::now_v7()).assert_ok();
		let query = base.with_alias("l").assert_ok().with_joins(vec![
			LiveJoin::new(
				LiveSource::GraphRecordExpr {
					graph: Box::new(LiveExpr::Binding(scope)),
					record: "other:a".parse().assert_ok(),
				},
				"r",
				LiveJoinKind::Left,
				Some(LiveExpr::Binding(predicate)),
			)
			.assert_ok(),
			LiveJoin::new(
				LiveSource::GraphExpr(Box::new(LiveExpr::Binding(scope))),
				"all",
				LiveJoinKind::Cross,
				None,
			)
			.assert_ok(),
		]);
		let mut expected = vec![scope, predicate];
		expected.sort_unstable();
		let options = crate::live::LiveBindingOptions::default();
		assert_eq!(
			crate::live::dependencies::query_dependencies(
				&query,
				&mut crate::live::dependencies::BindingAdmission::new(&options)
			)
			.assert_ok(),
			expected
		);
		assert_eq!(
			crate::live::dependencies::query_record_names(
				&query,
				&mut crate::live::dependencies::BindingAdmission::new(&options)
			)
			.assert_ok(),
			vec![crate::values::Text::from("other")]
		);
		let descriptor = LiveDescriptor::Query(query);
		let frame = super::super::encode(&descriptor, 1 << 20).assert_ok();
		assert_eq!(super::super::decode(&frame, 1 << 20).assert_ok(), descriptor);
		assert!(super::super::encode(&descriptor, 64).is_err());
		assert!(super::super::decode(&frame, frame.len()).is_err());
	}
}

//! Durable last-success String scopes of root JOIN declarations.

use super::codec::{Decoder, invalid};
use crate::{
	datastore::LiveRhsScopeAnchor,
	gql_status::FerrosGrynnResult,
	graph::GraphId,
	live::LiveQuery,
	storage::history_codec::CodecBudget,
	values::Text,
};

/// Validates sorted owners and borrowed names before allocating encoded bytes.
pub(super) fn size(anchors: &[LiveRhsScopeAnchor]) -> FerrosGrynnResult<usize> {
	if anchors.is_empty() {
		return Ok(0);
	}
	if anchors.windows(2).any(|pair| pair[0].source >= pair[1].source) {
		return Err(invalid("LIVE RHS anchor owners are not sorted distinct"));
	}
	anchors.iter().try_fold(4_usize, |bytes, anchor| {
		if anchor.source == 0
			|| anchor.name.is_empty()
			|| anchor.name.len() > 128
			|| anchor.name.as_str().contains('\0')
		{
			return Err(invalid("LIVE RHS anchor owner/name is invalid"));
		}
		bytes
			.checked_add(20)
			.and_then(|bytes| bytes.checked_add(anchor.name.len()))
			.ok_or_else(|| invalid("LIVE RHS anchor size overflows"))
	})
}

/// Appends a tail after the enclosing catalog has admitted its complete size.
pub(super) fn encode(
	output: &mut Vec<u8>,
	anchors: &[LiveRhsScopeAnchor],
) -> FerrosGrynnResult<()> {
	if anchors.is_empty() {
		return Ok(());
	}
	let _size = size(anchors)?;
	output.extend_from_slice(
		&u32::try_from(anchors.len())
			.map_err(|_error| invalid("LIVE RHS anchor count overflows"))?
			.to_le_bytes(),
	);
	for anchor in anchors {
		output.extend_from_slice(&anchor.source.to_le_bytes());
		output.extend_from_slice(&[0; 4]);
		output.extend_from_slice(&anchor.graph.get().to_le_bytes());
		output.extend_from_slice(
			&u32::try_from(anchor.name.len())
				.map_err(|_error| invalid("LIVE RHS anchor name length overflows"))?
				.to_le_bytes(),
		);
		output.extend_from_slice(anchor.name.as_str().as_bytes());
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::{decode, encode, size, validate};
	use crate::{
		datastore::{ClassId, LiveRhsScopeAnchor},
		gql_status::FerrosGrynnResult,
		graph::GraphId,
		live::{LiveExpr, LiveJoin, LiveJoinKind, LiveQuery, LiveSource},
		storage::{catalog::database::codec::Decoder, history_codec::CodecBudget},
	};

	/// Tail owners, lengths, clocks and reserved bytes never bypass admission.
	#[test]
	fn live_rhs_anchor_codec_checks_owners_and_bounds() -> FerrosGrynnResult<()> {
		let class = ClassId::from_storage(1)?;
		let graph = GraphId::new(2).ok_or_else(|| super::invalid("fixture graph is zero"))?;
		let query = LiveQuery::new(LiveSource::Class(class)).with_joins(vec![LiveJoin::new(
			LiveSource::GraphClassExpr {
				class,
				graph: Box::new(LiveExpr::Literal("scope".into())),
			},
			"r",
			LiveJoinKind::Cross,
			None,
		)?]);
		let anchors = vec![LiveRhsScopeAnchor {
			source: 1,
			graph,
			name: "scope".into(),
		}];
		let mut bytes = Vec::new();
		encode(&mut bytes, &anchors)?;
		assert_eq!(size(&anchors)?, 29);
		assert_eq!(&bytes[..4], &1_u32.to_le_bytes());
		assert_eq!(&bytes[4..12], &[1, 0, 0, 0, 0, 0, 0, 0]);
		assert_eq!(&bytes[12..20], &2_u64.to_le_bytes());
		assert_eq!(&bytes[20..24], &5_u32.to_le_bytes());
		assert_eq!(&bytes[24..], b"scope");
		assert_eq!(
			decode(&mut Decoder::new(&bytes), &mut CodecBudget::new(1 << 20, 0)?, &query)?,
			anchors
		);
		validate(&anchors, &query, 3)?;
		assert!(validate(&anchors, &query, 2).is_err());
		for (offset, value) in [
			(0, 0),
			(0, 255),
			(4, 0),
			(4, 2),
			(8, 1),
			(12, 0),
			(20, 0),
			(20, 129),
			(24, 0),
			(24, 255),
		] {
			let mut bad = bytes.clone();
			bad[offset] = value;
			assert!(
				decode(&mut Decoder::new(&bad), &mut CodecBudget::new(1 << 20, 0)?, &query)
					.is_err(),
				"offset {offset}"
			);
		}
		for length in 0..bytes.len() {
			assert!(
				decode(
					&mut Decoder::new(&bytes[..length]),
					&mut CodecBudget::new(1 << 20, 0)?,
					&query
				)
				.is_err()
			);
		}
		assert!(decode(&mut Decoder::new(&bytes), &mut CodecBudget::new(16, 0)?, &query).is_err());
		assert!(validate(&anchors, &LiveQuery::new(LiveSource::Class(class)), 3).is_err());
		Ok(())
	}
}

/// Counts and owner references are checked before retaining a decoded vector.
pub(super) fn decode(
	decoder: &mut Decoder<'_>,
	budget: &mut CodecBudget,
	query: &LiveQuery,
) -> FerrosGrynnResult<Vec<LiveRhsScopeAnchor>> {
	let count = decoder.read_count("LIVE RHS scope anchors")?;
	if count == 0 || count > query.joins().len() || count > decoder.remaining_bytes() / 21 {
		return Err(invalid("LIVE RHS anchor count is invalid"));
	}
	budget.admit(
		count
			.checked_mul(std::mem::size_of::<LiveRhsScopeAnchor>())
			.ok_or_else(|| invalid("LIVE RHS anchor capacity overflows"))?,
	)?;
	let mut anchors = Vec::new();
	anchors
		.try_reserve_exact(count)
		.map_err(|_error| invalid("LIVE RHS anchor allocation failed"))?;
	for _ in 0..count {
		let source = decoder.read_u32()?;
		decoder.expect_zero(4, "LIVE RHS anchor reserved bytes")?;
		let graph = GraphId::new(decoder.read_u64()?)
			.ok_or_else(|| invalid("LIVE RHS anchor graph is zero"))?;
		let length = decoder.read_count("LIVE RHS anchor name")?;
		if source == 0 || source as usize > query.joins().len() || length == 0 || length > 128 {
			return Err(invalid("LIVE RHS anchor reference/name length is invalid"));
		}
		budget.admit(length)?;
		let name = std::str::from_utf8(decoder.read_exact(length)?)
			.map_err(|_error| invalid("LIVE RHS anchor name is not UTF-8"))?;
		anchors.push(LiveRhsScopeAnchor {
			source,
			graph,
			name: Text::from(name),
		});
	}
	validate(&anchors, query, u64::MAX)?;
	Ok(anchors)
}

/// Retired graph identities remain valid metadata below the allocation clock.
pub(super) fn validate(
	anchors: &[LiveRhsScopeAnchor],
	query: &LiveQuery,
	next_graph: u64,
) -> FerrosGrynnResult<()> {
	let _size = size(anchors)?;
	for anchor in anchors {
		if anchor.graph.get() >= next_graph
			|| query
				.joins()
				.get(anchor.source as usize - 1)
				.is_none_or(|join| join.source().graph_expression().is_none())
		{
			return Err(invalid("LIVE RHS anchor disagrees with its source or allocation clock"));
		}
	}
	Ok(())
}

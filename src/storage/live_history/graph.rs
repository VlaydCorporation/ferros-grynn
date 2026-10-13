//! Graph payloads reuse independent checkpoints and aggregate history budgets.

use std::{cell::RefCell, sync::Arc};

use super::{
	CodecBudget,
	DateTime,
	Decoder,
	FerrosGrynnResult,
	LiveBindingId,
	LiveBindingRevision,
	LiveGroupInfo,
	LiveId,
	LiveVersion,
	MAX_NODES,
	Work,
	append,
	append_blob,
	invalid,
	values,
};
use crate::{
	live::{LiveGraphBatch, LiveGraphDelta, LiveGraphImage},
	storage::live_graph_image,
};

/// Recomputes canonical targets instead of trusting a second serialized list.
fn delta(
	before: Option<Arc<LiveGraphImage>>,
	after: Arc<LiveGraphImage>,
	base: u64,
	revision: u64,
	budget: &mut CodecBudget,
	work: &mut Work,
) -> FerrosGrynnResult<LiveGraphDelta> {
	for (image, maximum) in before
		.as_deref()
		.map(|image| (image, base))
		.into_iter()
		.chain(std::iter::once((after.as_ref(), revision)))
	{
		for scope in image.scopes.values() {
			work.take(
				0,
				scope
					.nodes
					.len()
					.checked_add(scope.edges.len())
					.ok_or_else(|| invalid("graph source count overflow"))?,
			)?;
			for source in scope
				.nodes
				.values()
				.filter_map(|node| node.source())
				.chain(scope.edges.values().filter_map(|edge| edge.source()))
			{
				if source.revision().get() > maximum {
					return Err(invalid("graph source snapshot exceeds its graph watermark"));
				}
			}
		}
	}
	let limits = RefCell::new((budget, work));
	LiveGraphDelta::between(before, after, base.into(), revision.into(), &|bytes, units| {
		let mut limits = limits.borrow_mut();
		limits.0.admit(bytes)?;
		limits.1.take(
			0,
			usize::try_from(units).map_err(|error| {
				invalid("graph delta work exceeds usize").with_technical_source(error)
			})?,
		)
	})
}

/// Checks historical reactive revisions independently of primary records.
fn validate_bindings(bindings: &[LiveBindingRevision], stamp: u64) -> FerrosGrynnResult<()> {
	if bindings.len() > MAX_NODES
		|| bindings.windows(2).any(|pair| pair[0].id >= pair[1].id)
		|| bindings.iter().any(|binding| binding.value.get() == 0 || binding.value.get() > stamp)
	{
		return Err(invalid("LIVE graph binding revisions are invalid"));
	}
	Ok(())
}

/// Encodes one complete graph commit, including its preceding checkpoint.
#[allow(clippy::too_many_arguments, reason = "matches the existing checked publication envelope")]
pub(super) fn encode(
	body: &mut Vec<u8>,
	batch: &LiveGraphBatch,
	id: LiveId,
	version: LiveVersion,
	stamp: u64,
	time: i128,
	budget: &mut CodecBudget,
	work: &mut Work,
) -> FerrosGrynnResult<()> {
	if batch.id() != id
		|| batch.version() != version
		|| batch.versionstamp().get() != stamp
		|| batch.committed_at().as_inner().timestamp().as_nanosecond() != time
	{
		return Err(invalid("LIVE graph differs from its publication envelope"));
	}
	validate_bindings(batch.bindings(), stamp)?;
	let graph = batch.delta();
	let expected = delta(
		graph.before.clone(),
		Arc::clone(&graph.after),
		graph.base().get(),
		stamp,
		budget,
		work,
	)?;
	if expected.changes() != graph.changes() || graph.before().is_none() && graph.base().get() != 0
	{
		return Err(invalid("LIVE graph delta targets or initial base are invalid"));
	}
	work.take(0, batch.bindings().len())?;
	for binding in batch.bindings() {
		append(body, binding.id.as_uuid().as_inner().as_bytes(), budget)?;
		append(body, &binding.definition.get().to_le_bytes(), budget)?;
		append(body, &binding.value.get().to_le_bytes(), budget)?;
	}
	append(body, &graph.base().get().to_le_bytes(), budget)?;
	append(body, &[u8::from(graph.before().is_some())], budget)?;
	if let Some(before) = graph.before() {
		let bytes = live_graph_image::encode_bounded(before, budget, work)?;
		append_blob(body, &bytes, budget)?;
	}
	let bytes = live_graph_image::encode_bounded(graph.after(), budget, work)?;
	append_blob(body, &bytes, budget)
}

/// Restores both images before deriving and validating the whole delta.
pub(super) fn decode(
	body: &mut Decoder<'_>,
	info: LiveGroupInfo,
	bindings: usize,
	budget: &mut CodecBudget,
	work: &mut Work,
) -> FerrosGrynnResult<LiveGraphBatch> {
	work.take(0, bindings)?;
	let mut revisions = values::reserve::<LiveBindingRevision>(bindings, budget)?;
	for _ in 0..bindings {
		let id = LiveBindingId::from_uuid(
			uuid::Uuid::from_slice(body.take(16)?)
				.map_err(|error| {
					invalid("LIVE graph binding identity is malformed").with_technical_source(error)
				})?
				.into(),
		)?;
		revisions.push(LiveBindingRevision {
			id,
			definition: LiveVersion::new(body.u64()?)?,
			value: body.u64()?.into(),
		});
	}
	validate_bindings(&revisions, info.stamp)?;
	let base = body.u64()?;
	let before = match body.u8()? {
		0 if base == 0 => None,
		1 => Some(Arc::new(live_graph_image::decode_bounded(body.blob()?, budget, work)?)),
		_ => return Err(invalid("LIVE graph prior checkpoint presence/base is invalid")),
	};
	let after = Arc::new(live_graph_image::decode_bounded(body.blob()?, budget, work)?);
	let delta = delta(before, after, base, info.stamp, budget, work)?;
	if delta.changes().len().max(1) != info.events as usize {
		return Err(invalid("LIVE graph delta count differs from its header"));
	}
	Ok(LiveGraphBatch {
		id: info.id,
		version: info.version,
		committed_at: DateTime::from(
			jiff::Timestamp::from_nanosecond(info.time)?.to_zoned(jiff::tz::TimeZone::UTC),
		),
		delta,
		bindings: revisions,
	})
}

#[cfg(test)]
mod tests {
	use std::collections::BTreeMap;

	use super::{super::*, *};
	use crate::{
		graph::{GraphKind, GraphTopologyConstraints},
		live::{LiveGraphScope, LiveGraphScopeId},
		values::Object,
	};

	/// Empty graph revisions are still data, and preserve their full
	/// checkpoint.
	#[test]
	fn graph_history_roundtrip_checks_framing_counts_and_limits() -> FerrosGrynnResult<()> {
		let mut properties = Object::new();
		properties.insert("payload", Value::from("x".repeat(70 * 1024)));
		let image = Arc::new(LiveGraphImage {
			family: GraphKind::Graph,
			validators: GraphTopologyConstraints::empty(),
			root: None,
			property_keys: BTreeMap::from([(
				"payload".into(),
				crate::values::PropertyKeyId::new(0),
			)]),
			scopes: BTreeMap::from([(LiveGraphScopeId::Root, LiveGraphScope {
				source: None,
				validators: GraphTopologyConstraints::empty(),
				properties,
				nodes: BTreeMap::new(),
				edges: BTreeMap::new(),
			})]),
		});
		let delta =
			LiveGraphDelta::between(None, Arc::clone(&image), 0.into(), 5.into(), &|_, _| Ok(()))?;
		let batch = LiveGraphBatch {
			id: "11111111-1111-1111-1111-111111111111".parse()?,
			version: LiveVersion::new(1)?,
			committed_at: DateTime::from(
				jiff::Timestamp::from_nanosecond(100)?.to_zoned(jiff::tz::TimeZone::UTC),
			),
			delta,
			bindings: Vec::new(),
		};
		let encoded = super::super::encode(
			&LivePayload::Graph(Arc::new(batch.clone())),
			batch.id(),
			batch.version(),
			5,
			100,
			8 << 20,
		)?;
		assert_eq!(&encoded[..8], b"FGLB\x02\x00\x03\x00");
		assert!(encoded.len() > CONTROL_BYTES);
		assert!(!inspect(&encoded, encoded.len() as u64)?.is_control());
		let LivePayload::Graph(restored) = decode_delivery(&encoded, 8 << 20)? else {
			return Err(invalid("graph result kind was lost"));
		};
		assert_eq!(restored.delta().after().scopes, image.scopes);
		assert_eq!(restored.delta().changes(), batch.delta().changes());
		assert_eq!(
			super::super::encode(
				&LivePayload::Graph(restored),
				batch.id(),
				batch.version(),
				5,
				100,
				8 << 20
			)?,
			encoded
		);
		assert!(decode_delivery(&encoded, encoded.len()).is_err());
		for offset in [4_usize, 6, 68, 72, 80, encoded.len() - 1] {
			let mut corrupt = encoded.clone();
			corrupt[offset] ^= 0xff;
			assert!(decode_delivery(&corrupt, 8 << 20).is_err(), "accepted corruption at {offset}");
		}
		let delta = LiveGraphDelta::between(
			Some(Arc::clone(&image)),
			image,
			5.into(),
			6.into(),
			&|_, _| Ok(()),
		)?;
		assert!(delta.changes().is_empty());
		let mut quiet = batch;
		quiet.delta = delta;
		let encoded = super::super::encode(
			&LivePayload::Graph(Arc::new(quiet.clone())),
			quiet.id(),
			quiet.version(),
			6,
			100,
			8 << 20,
		)?;
		assert_eq!(inspect(&encoded, encoded.len() as u64)?.events, 1);
		assert!(
			matches!(decode_delivery(&encoded, 8 << 20)?, LivePayload::Graph(batch) if batch.delta().changes().is_empty())
		);
		Ok(())
	}
}

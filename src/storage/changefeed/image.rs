//! Exact global and qualified images with ordered endpoint containment routes.

use std::{collections::BTreeMap, sync::Arc};

use super::{CodecBudget, Decoder, append, append_blob, invalid, values};
use crate::{
	changefeed::{ChangeEndpoint, ChangeGraphState, ChangeIdentity, ChangeImage, ChangeOverlay},
	gql_status::FerrosGrynnResult,
	graph::{AtomKind, AtomOccurrenceRef, AtomRef, EdgeDirection, GraphRef},
	values::{Object, Text, Value, storage::EncodeContext},
};

/// Encodes one atom through the canonical stable RecordId representation.
fn encode_atom(
	output: &mut Vec<u8>,
	atom: &AtomRef,
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<()> {
	budget.admit(
		128usize
			.checked_add(atom.id().estimated_heap_bytes())
			.ok_or_else(|| invalid("changefeed atom reservation overflows"))?,
	)?;
	let bytes = values::encode(&Value::from(atom.clone()), context, budget)?;
	append_blob(output, &bytes, budget)
}

/// Decodes a canonical atom reference, rejecting other persistable value kinds.
fn decode_atom(
	decoder: &mut Decoder<'_>,
	context: &EncodeContext,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<AtomRef> {
	match values::decode(decoder.blob()?, context, budget)? {
		Value::Atom(atom) => Ok(*atom),
		_ => Err(invalid("changefeed identity is not an atom reference")),
	}
}

/// Writes a qualified occurrence without materialization-local handles.
fn encode_occurrence(
	output: &mut Vec<u8>,
	occurrence: &AtomOccurrenceRef,
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<()> {
	let slot = values::graph_slot(context, occurrence.graph(), budget)?;
	append(output, &slot.to_le_bytes(), budget)?;
	encode_atom(output, occurrence.atom(), context, budget)
}

/// Resolves one qualified historical occurrence from this group's dictionaries.
fn decode_occurrence(
	decoder: &mut Decoder<'_>,
	context: &EncodeContext,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<AtomOccurrenceRef> {
	let graph = values::graph(context, decoder.u64()?, budget)?;
	let atom = decode_atom(decoder, context, budget)?;
	budget.admit(std::mem::size_of::<GraphRef>() + 32)?;
	Ok(AtomOccurrenceRef::new(atom, Arc::new(graph)))
}

/// Writes optional graph bindings with zero reserved for absence.
fn encode_graph(
	output: &mut Vec<u8>,
	graph: Option<&GraphRef>,
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<()> {
	let slot =
		graph.map(|graph| values::graph_slot(context, graph, budget)).transpose()?.unwrap_or(0);
	append(output, &slot.to_le_bytes(), budget)
}

/// Decodes a nullable historical graph slot without reading primary metadata.
fn decode_graph(
	decoder: &mut Decoder<'_>,
	context: &EncodeContext,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<Option<GraphRef>> {
	let slot = decoder.u64()?;
	if slot == 0 {
		Ok(None)
	} else {
		values::graph(context, slot, budget).map(Some)
	}
}

/// Preserves endpoint order, multiplicity and every qualified owner in its
/// route.
fn encode_endpoints(
	output: &mut Vec<u8>,
	endpoints: &[ChangeEndpoint],
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<()> {
	values::write_count(output, endpoints.len(), budget)?;
	for endpoint in endpoints {
		encode_occurrence(output, endpoint.occurrence(), context, budget)?;
		values::write_count(output, endpoint.owners().len(), budget)?;
		for owner in endpoint.owners() {
			encode_occurrence(output, owner, context, budget)?;
		}
	}
	Ok(())
}

/// Admits complete endpoint and route arrays before allocating from counts.
fn decode_endpoints(
	decoder: &mut Decoder<'_>,
	context: &EncodeContext,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<Vec<ChangeEndpoint>> {
	let count = values::count(decoder, 20)?;
	budget.admit(
		count
			.checked_mul(std::mem::size_of::<ChangeEndpoint>())
			.ok_or_else(|| invalid("changefeed endpoint allocation overflows"))?,
	)?;
	let mut output = Vec::new();
	output
		.try_reserve_exact(count)
		.map_err(|_error| invalid("changefeed endpoint allocation failed"))?;
	for _index in 0..count {
		let occurrence = decode_occurrence(decoder, context, budget)?;
		let count = values::count(decoder, 16)?;
		if count > 64 {
			return Err(invalid("changefeed endpoint containment depth exceeds 64"));
		}
		budget.admit(count * std::mem::size_of::<AtomOccurrenceRef>())?;
		let mut owners = Vec::new();
		owners
			.try_reserve_exact(count)
			.map_err(|_error| invalid("changefeed owner route allocation failed"))?;
		for _index in 0..count {
			owners.push(decode_occurrence(decoder, context, budget)?);
		}
		output.push(ChangeEndpoint {
			occurrence,
			owners,
		});
	}
	Ok(output)
}

/// Serializes an image independently of the current primary records and schema.
pub(super) fn encode(
	image: &ChangeImage,
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<Vec<u8>> {
	validate(image)?;
	let mut output = Vec::new();
	let kind = u8::from(matches!(image.identity(), ChangeIdentity::Occurrence(_)));
	let state = match image.graph_state() {
		None => 0,
		Some(ChangeGraphState::Node {
			..
		}) => 1,
		Some(ChangeGraphState::Edge {
			..
		}) => 2,
	};
	append(&mut output, &[kind, state, u8::from(image.label().is_some()), 0, 0, 0, 0, 0], budget)?;
	match image.identity() {
		ChangeIdentity::Record(atom) => encode_atom(&mut output, atom, context, budget)?,
		ChangeIdentity::Occurrence(occurrence) => {
			encode_occurrence(&mut output, occurrence, context, budget)?;
		}
	}
	values::write_count(&mut output, image.properties().len(), budget)?;
	for (name, value) in image.properties() {
		let slot = values::property_slot(context, name, budget)?;
		append(&mut output, &slot.to_le_bytes(), budget)?;
		let bytes = values::encode(value, context, budget)?;
		append_blob(&mut output, &bytes, budget)?;
	}
	values::write_count(&mut output, image.overlays().len(), budget)?;
	for (name, decision) in image.overlays() {
		let slot = values::property_slot(context, name, budget)?;
		append(&mut output, &slot.to_le_bytes(), budget)?;
		append(
			&mut output,
			&[u8::from(matches!(decision, ChangeOverlay::Value(_))), 0, 0, 0],
			budget,
		)?;
		if let ChangeOverlay::Value(value) = decision {
			let bytes = values::encode(value, context, budget)?;
			append_blob(&mut output, &bytes, budget)?;
		}
	}
	if let Some(label) = image.label() {
		values::append_text(&mut output, label, budget)?;
	}
	match image.graph_state() {
		None => {}
		Some(ChangeGraphState::Node {
			nested_graph,
		}) => encode_graph(&mut output, nested_graph.as_ref(), context, budget)?,
		Some(ChangeGraphState::Edge {
			direction,
			source,
			target,
			weight,
			transition_graph,
		}) => {
			let direction = match direction {
				EdgeDirection::Directed => 0,
				EdgeDirection::Bidirectional => 1,
				EdgeDirection::Undirected => 2,
			};
			append(
				&mut output,
				&[direction, u8::from(weight.is_some()), 0, 0, 0, 0, 0, 0],
				budget,
			)?;
			encode_graph(&mut output, transition_graph.as_ref(), context, budget)?;
			if let Some(weight) = weight {
				let bytes = values::encode(&Value::Number(*weight), context, budget)?;
				append_blob(&mut output, &bytes, budget)?;
			}
			encode_endpoints(&mut output, source, context, budget)?;
			encode_endpoints(&mut output, target, context, budget)?;
		}
	}
	Ok(output)
}

/// Restores complete public image ownership from one checked private image.
pub(super) fn decode(
	bytes: &[u8],
	context: &EncodeContext,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<Arc<ChangeImage>> {
	let mut decoder = Decoder::new(bytes);
	let kind = decoder.u8()?;
	let state = decoder.u8()?;
	let label = decoder.u8()?;
	decoder.zero(5)?;
	let identity = match kind {
		0 => ChangeIdentity::Record(decode_atom(&mut decoder, context, budget)?),
		1 => ChangeIdentity::Occurrence(decode_occurrence(&mut decoder, context, budget)?),
		_ => return Err(invalid("changefeed image identity kind is unknown")),
	};
	let count = values::count(&mut decoder, 12)?;
	budget.admit(crate::values::storage::tree_allocation_bytes(
		count,
		std::mem::size_of::<(Text, Value)>(),
	)?)?;
	let mut properties = Object::new();
	for _index in 0..count {
		let name = values::property(context, decoder.u32()?, budget)?;
		ordered_name(properties.as_map().last_key_value().map(|(name, _value)| name), &name)?;
		let value = values::decode(decoder.blob()?, context, budget)?;
		properties.insert(name, value);
	}
	let count = values::count(&mut decoder, 8)?;
	budget.admit(crate::values::storage::tree_allocation_bytes(
		count,
		std::mem::size_of::<(Text, ChangeOverlay)>(),
	)?)?;
	let mut overlays = BTreeMap::new();
	for _index in 0..count {
		let name = values::property(context, decoder.u32()?, budget)?;
		ordered_name(overlays.last_key_value().map(|(name, _value)| name), &name)?;
		let kind = decoder.u8()?;
		decoder.zero(3)?;
		let decision = match kind {
			0 => ChangeOverlay::Hidden,
			1 => ChangeOverlay::Value(values::decode(decoder.blob()?, context, budget)?),
			_ => return Err(invalid("changefeed overlay kind is unknown")),
		};
		overlays.insert(name, decision);
	}
	let label = match label {
		0 => None,
		1 => Some(values::text(&mut decoder, budget)?),
		_ => return Err(invalid("changefeed label presence flag is unknown")),
	};
	let graph_state = match state {
		0 => None,
		1 => Some(ChangeGraphState::Node {
			nested_graph: decode_graph(&mut decoder, context, budget)?,
		}),
		2 => {
			let direction = match decoder.u8()? {
				0 => EdgeDirection::Directed,
				1 => EdgeDirection::Bidirectional,
				2 => EdgeDirection::Undirected,
				_ => return Err(invalid("changefeed edge direction is unknown")),
			};
			let weighted = decoder.u8()?;
			decoder.zero(6)?;
			let transition_graph = decode_graph(&mut decoder, context, budget)?;
			let weight = match weighted {
				0 => None,
				1 => match values::decode(decoder.blob()?, context, budget)? {
					Value::Number(number) => Some(number),
					_ => return Err(invalid("changefeed edge weight is not numeric")),
				},
				_ => return Err(invalid("changefeed edge weight presence flag is unknown")),
			};
			let source = decode_endpoints(&mut decoder, context, budget)?;
			let target = decode_endpoints(&mut decoder, context, budget)?;
			Some(ChangeGraphState::Edge {
				direction,
				source,
				target,
				weight,
				transition_graph,
			})
		}
		_ => return Err(invalid("changefeed graph state kind is unknown")),
	};
	decoder.finish()?;
	budget.admit(std::mem::size_of::<ChangeImage>() + 32)?;
	let image = ChangeImage {
		identity,
		properties,
		label,
		overlays,
		graph_state,
	};
	validate(&image)?;
	Ok(Arc::new(image))
}

/// Rejects duplicates and noncanonical named-property order before insertion.
fn ordered_name(previous: Option<&Text>, name: &Text) -> FerrosGrynnResult<()> {
	if previous.is_some_and(|previous| previous >= name) {
		return Err(invalid("changefeed image property names are not strictly ordered"));
	}
	Ok(())
}

/// Checks CLASS/GRAPH shape and endpoint constraints independently of encoding.
fn validate(image: &ChangeImage) -> FerrosGrynnResult<()> {
	let kind = match image.identity() {
		ChangeIdentity::Record(_) => {
			if image.label().is_some()
				|| !image.overlays().is_empty()
				|| image.graph_state().is_some()
			{
				return Err(invalid("changefeed CLASS image contains graph-local fields"));
			}
			return Ok(());
		}
		ChangeIdentity::Occurrence(occurrence) => occurrence.atom().kind(),
	};
	match (kind, image.graph_state()) {
		(
			AtomKind::Node,
			Some(ChangeGraphState::Node {
				..
			}),
		) => Ok(()),
		(
			AtomKind::Edge,
			Some(ChangeGraphState::Edge {
				direction,
				source,
				target,
				..
			}),
		) => {
			if source.is_empty()
				|| matches!(direction, EdgeDirection::Undirected) != target.is_empty()
			{
				return Err(invalid("changefeed connector topology is invalid"));
			}
			if source.iter().chain(target).any(|endpoint| endpoint.owners().len() > 64) {
				return Err(invalid("changefeed endpoint containment depth exceeds 64"));
			}
			Ok(())
		}
		_ => Err(invalid("changefeed graph image atom kind and structural state disagree")),
	}
}

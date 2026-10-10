//! Historical dictionary slots around the authoritative canonical value codec.

use std::ops::Bound;

use super::{CodecBudget, Decoder, append, append_blob, invalid};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{GraphId, GraphKind, GraphRef, GraphStorageMode},
	values::{
		RecordIdPart,
		Text,
		Value,
		storage::{
			DiskGraphRef,
			EncodeContext,
			StorageValue,
			codec::{CanonicalCodec, DecodeLimits},
			tree_allocation_bytes,
		},
	},
};

/// Upper bound for all group-local dictionary and collection counts.
const MAX_COUNT: usize = 1_000_000;

/// Checks counts against both an independent cap and available encoded bytes.
pub(in crate::storage) fn count(
	decoder: &mut Decoder<'_>,
	minimum: usize,
) -> FerrosGrynnResult<usize> {
	let count = decoder.u32()? as usize;
	if count > MAX_COUNT || count > (decoder.bytes.len() - decoder.position) / minimum {
		return Err(invalid("changefeed collection count exceeds its encoded bounds"));
	}
	Ok(count)
}

/// Writes a checked fixed count, never narrowing silently.
pub(in crate::storage) fn write_count(
	output: &mut Vec<u8>,
	count: usize,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<()> {
	if count > MAX_COUNT {
		return Err(invalid("changefeed collection count exceeds its cap"));
	}
	let count = u32::try_from(count).map_err(|_error| invalid("changefeed count exceeds u32"))?;
	append(output, &count.to_le_bytes(), budget)
}

/// Admits dictionary-name copies before their owning runtime allocation.
pub(in crate::storage) fn text(
	decoder: &mut Decoder<'_>,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<Text> {
	let bytes = decoder.blob()?;
	if bytes.len() > 16 * 1024 * 1024 {
		return Err(invalid("changefeed text exceeds its cap"));
	}
	let name =
		std::str::from_utf8(bytes).map_err(|_error| invalid("changefeed text is not UTF-8"))?;
	budget.admit(
		bytes
			.len()
			.checked_mul(2)
			.and_then(|bytes| bytes.checked_add(256))
			.ok_or_else(|| invalid("changefeed text reservation overflows"))?,
	)?;
	Ok(name.into())
}

/// Registers a complete historical graph descriptor under a group-local slot.
pub(in crate::storage) fn graph_slot(
	context: &mut EncodeContext,
	graph: &GraphRef,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<u64> {
	if let Some((slot, _)) =
		context.graph_references().find(|(_slot, reference)| *reference == graph)
	{
		return Ok(slot.raw());
	}
	let ordinal = context
		.graph_references()
		.len()
		.checked_add(1)
		.ok_or_else(|| invalid("changefeed graph dictionary count overflows"))?;
	if ordinal > MAX_COUNT {
		return Err(invalid("changefeed graph dictionary exceeds its cap"));
	}
	admit_dictionary_growth(budget, ordinal, std::mem::size_of::<(GraphRef, DiskGraphRef)>())?;
	budget.admit(512 + graph.name().len() * 2)?;
	let slot =
		GraphId::new(ordinal as u64).ok_or_else(|| invalid("changefeed graph slot is zero"))?;
	context.register_historical_graph(graph.clone(), DiskGraphRef::new(slot))?;
	Ok(ordinal as u64)
}

/// Resolves a historical dictionary slot without reading a current catalog.
pub(in crate::storage) fn graph(
	context: &EncodeContext,
	slot: u64,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<GraphRef> {
	let slot = GraphId::new(slot).ok_or_else(|| invalid("changefeed graph slot is zero"))?;
	let graph = context
		.registered_graph(DiskGraphRef::new(slot))
		.ok_or_else(|| invalid("changefeed graph slot is unresolved"))?;
	budget.admit(128 + graph.name().len())?;
	Ok(graph.clone())
}

/// Registers a property name without copying its owning value.
pub(in crate::storage) fn property_slot(
	context: &mut EncodeContext,
	name: &Text,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<u32> {
	check_text(name.len())?;
	if let Some(key) = context.property_dictionary().get(name.as_str()) {
		return Ok(key.get());
	}
	let ordinal = context.property_dictionary().len();
	if ordinal >= MAX_COUNT {
		return Err(invalid("changefeed property dictionary exceeds its cap"));
	}
	budget.admit(256 + name.len() * 2)?;
	context.register_property_name((ordinal as u32).into(), name.clone())?;
	Ok(ordinal as u32)
}

/// Bounds both directions of a historical B-tree dictionary before insertion.
fn dictionary_tree_bytes(count: usize, entry_bytes: usize) -> FerrosGrynnResult<usize> {
	tree_allocation_bytes(count, entry_bytes)?
		.checked_mul(2)
		.ok_or_else(|| invalid("changefeed dictionary tree reservation overflows"))
}

/// Admits complete first nodes plus checked incremental dictionary growth.
fn admit_dictionary_growth(
	budget: &mut CodecBudget,
	count: usize,
	entry_bytes: usize,
) -> FerrosGrynnResult<()> {
	let new = dictionary_tree_bytes(count, entry_bytes)?;
	let previous = dictionary_tree_bytes(
		count.checked_sub(1).ok_or_else(|| invalid("changefeed dictionary growth is zero"))?,
		entry_bytes,
	)?;
	budget.admit(
		new.checked_sub(previous)
			.ok_or_else(|| invalid("changefeed dictionary growth reservation decreases"))?,
	)
}

/// Resolves and admits an owned property name before insertion into an image.
pub(in crate::storage) fn property(
	context: &EncodeContext,
	slot: u32,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<Text> {
	let name = context
		.property_dictionary()
		.resolve(slot.into())
		.ok_or_else(|| invalid("changefeed property slot is unresolved"))?;
	budget.admit(256 + name.len())?;
	Ok(name.clone())
}

/// Admits clones, conversion, dictionary growth and canonical encode scratch.
/// The per-node reservation includes temporary set ordering buffers.
fn prepare(
	value: &Value,
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
	depth: usize,
	observe: Option<&dyn Fn(u64) -> FerrosGrynnResult<()>>,
) -> FerrosGrynnResult<()> {
	observe_value(value, observe)?;
	if depth > 64 {
		return Err(invalid("changefeed value nesting exceeds 64"));
	}
	budget.admit(512)?;
	match value {
		Value::Array(values) => {
			check_count(values.len())?;
			for value in values {
				prepare(value, context, budget, depth + 1, observe)?;
			}
		}
		Value::Set(values) => {
			check_count(values.len())?;
			for value in values {
				prepare(value, context, budget, depth + 1, observe)?;
			}
		}
		Value::Object(values) => {
			check_count(values.len())?;
			for (name, value) in values {
				if let Some(observe) = observe {
					observe(name.len() as u64)?;
				}
				check_text(name.len())?;
				budget.admit(
					name.len()
						.checked_mul(8)
						.ok_or_else(|| invalid("changefeed property name reservation overflows"))?,
				)?;
				prepare(value, context, budget, depth + 1, observe)?;
			}
		}
		Value::Range(range) => {
			for bound in [range.start(), range.end()] {
				if let Bound::Included(value) | Bound::Excluded(value) = bound {
					prepare(value, context, budget, depth + 1, observe)?;
				}
			}
		}
		Value::Record(record) => {
			prepare_record(record, context, budget, depth + 1, observe)?;
		}
		Value::Atom(atom) => {
			prepare_record(atom.id(), context, budget, depth + 1, observe)?;
		}
		Value::GraphRef(graph) => {
			graph_slot(context, graph, budget)?;
		}
		Value::Geometry(geometry) => {
			check_geometry_depth(geometry, depth)?;
			budget.admit(geometry.validation_scratch_bytes()?)?;
			geometry.validate()?;
		}
		Value::String(value) => check_text(value.len())?,
		Value::DateTime(value) => {
			if let Some(name) =
				value.as_inner().time_zone().iana_name().filter(|name| *name != "UTC")
			{
				prepare_time_zone(name, context, budget)?;
			}
		}
		Value::Bytes(value) => {
			if value.len() > 64 * 1024 * 1024 {
				return Err(invalid("changefeed canonical blob exceeds its cap"));
			}
		}
		Value::None
		| Value::Null
		| Value::Bool(_)
		| Value::Number(_)
		| Value::Duration(_)
		| Value::Uuid(_)
		| Value::Ulid(_)
		| Value::Vector(_) => {}
		Value::Regex(_)
		| Value::Matcher(_)
		| Value::AtomOccurrence(_)
		| Value::Path(_)
		| Value::Graph(_)
		| Value::Closure(_)
		| Value::Error(_)
		| Value::Cursor(_) => return Err(invalid("changefeed property contains a runtime-only value")),
	}
	Ok(())
}

/// Checks a borrowed record before constructing an owning value envelope.
///
/// # Errors
///
/// Returns depth, count, dictionary, byte or caller admission errors.
pub(in crate::storage) fn prepare_record(
	record: &crate::values::RecordId,
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
	depth: usize,
	observe: Option<&dyn Fn(u64) -> FerrosGrynnResult<()>>,
) -> FerrosGrynnResult<()> {
	prepare_record_part(record.id(), budget, depth, observe)?;
	if let Some(observe) = observe {
		observe(record.class_text().len() as u64)?;
	}
	register_class(context, record.class_text(), budget)
}

/// Admits one canonical node and its flat payload before cloning or validation.
fn observe_value(
	value: &Value,
	observe: Option<&dyn Fn(u64) -> FerrosGrynnResult<()>>,
) -> FerrosGrynnResult<()> {
	let Some(observe) = observe else {
		return Ok(());
	};
	let bytes = match value {
		Value::String(text) => text.len(),
		Value::Bytes(bytes) => bytes.len(),
		Value::Vector(vector) => vector.len(),
		Value::Geometry(geometry) => geometry.validation_scratch_bytes()?,
		Value::GraphRef(graph) => graph.name().len(),
		Value::Record(record) => record.class_text().len(),
		Value::Atom(atom) => atom.id().class_text().len(),
		_ => 0,
	};
	observe((bytes as u64).saturating_add(1))
}

/// Admits arbitrary named TZif identities before canonical conversion copies
/// their names; UTC and fixed offsets require no dictionary allocation.
pub(in crate::storage) fn prepare_time_zone(
	name: &str,
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<()> {
	check_text(name.len())?;
	let copies = name
		.len()
		.checked_mul(3)
		.and_then(|bytes| bytes.checked_add(512))
		.ok_or_else(|| invalid("changefeed time-zone reservation overflows"))?;
	budget.admit(copies)?;
	if context.time_zone_names().any(|existing| existing.as_str() == name) {
		return Ok(());
	}
	let count = context.time_zone_names().len();
	check_count(
		count.checked_add(1).ok_or_else(|| invalid("changefeed time-zone count overflows"))?,
	)?;
	let new = tree_allocation_bytes(count + 1, std::mem::size_of::<(Text, u32)>())?;
	let previous = tree_allocation_bytes(count, std::mem::size_of::<(Text, u32)>())?;
	budget.admit(
		new.checked_sub(previous)
			.ok_or_else(|| invalid("changefeed time-zone tree growth decreases"))?,
	)?;
	context.register_time_zone_name(name.into())?;
	Ok(())
}

/// Shares canonical encoder/decoder collection bounds before cloning inputs.
fn check_count(count: usize) -> FerrosGrynnResult<()> {
	if count > MAX_COUNT {
		return Err(FerrosGrynnError::storage_decode_limit_exceeded(
			"changefeed canonical collection",
			count as u64,
			MAX_COUNT as u64,
		));
	}
	Ok(())
}

/// Shares the canonical decoder's owned UTF-8 text cap.
fn check_text(bytes: usize) -> FerrosGrynnResult<()> {
	if bytes > 16 * 1024 * 1024 {
		return Err(FerrosGrynnError::storage_decode_limit_exceeded(
			"changefeed canonical text",
			bytes as u64,
			16 * 1024 * 1024,
		));
	}
	Ok(())
}

/// Applies the shared text cap to outer image names/labels and dictionaries.
pub(in crate::storage) fn append_text(
	output: &mut Vec<u8>,
	text: &Text,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<()> {
	check_text(text.len())?;
	append_blob(output, text.as_bytes(), budget)
}

/// Checks composite RecordId values before their temporary DTO allocation.
fn prepare_record_part(
	part: &RecordIdPart,
	budget: &mut CodecBudget,
	depth: usize,
	observe: Option<&dyn Fn(u64) -> FerrosGrynnResult<()>>,
) -> FerrosGrynnResult<()> {
	if let Some(observe) = observe {
		observe(1)?;
	}
	check_depth(depth)?;
	match part {
		RecordIdPart::Array(values) => {
			check_count(values.len())?;
			for value in values {
				prepare_record_value(value, budget, depth + 1, observe)?;
			}
		}
		RecordIdPart::Object(values) => {
			check_depth(depth + 1)?;
			check_count(values.len())?;
			for (name, value) in values {
				if let Some(observe) = observe {
					observe(name.len() as u64)?;
				}
				check_text(name.len())?;
				budget.admit(name.len() * 8)?;
				prepare_record_value(value, budget, depth + 2, observe)?;
			}
		}
		RecordIdPart::String(value) => {
			if let Some(observe) = observe {
				observe(value.len() as u64)?;
			}
			check_text(value.len())?;
		}
		RecordIdPart::Int(_) | RecordIdPart::Uuid(_) | RecordIdPart::Ulid(_) => {}
	}
	Ok(())
}

/// Matches canonical RecordId's extra object section depth before cloning.
fn prepare_record_value(
	value: &Value,
	budget: &mut CodecBudget,
	depth: usize,
	observe: Option<&dyn Fn(u64) -> FerrosGrynnResult<()>>,
) -> FerrosGrynnResult<()> {
	observe_value(value, observe)?;
	check_depth(depth)?;
	budget.admit(512)?;
	match value {
		Value::Array(values) => {
			check_count(values.len())?;
			for value in values {
				prepare_record_value(value, budget, depth + 1, observe)?;
			}
		}
		Value::Object(values) => {
			check_depth(depth + 1)?;
			check_count(values.len())?;
			for (name, value) in values {
				if let Some(observe) = observe {
					observe(name.len() as u64)?;
				}
				check_text(name.len())?;
				budget.admit(name.len() * 8)?;
				prepare_record_value(value, budget, depth + 2, observe)?;
			}
		}
		Value::String(value) => check_text(value.len())?,
		Value::Number(crate::values::Number::Int(_)) | Value::Uuid(_) | Value::Ulid(_) => {}
		_ => return Err(invalid("changefeed composite RecordId contains an invalid value")),
	}
	Ok(())
}

/// Shares canonical value and geometry recursion's absolute depth ceiling.
fn check_depth(depth: usize) -> FerrosGrynnResult<()> {
	if depth > 64 {
		return Err(invalid("changefeed canonical nesting exceeds 64"));
	}
	Ok(())
}

/// Accounts for value-container depth around geometry's collection recursion.
fn check_geometry_depth(geometry: &crate::values::Geometry, depth: usize) -> FerrosGrynnResult<()> {
	check_depth(depth)?;
	if let crate::values::Geometry::Collection(values) = geometry {
		for value in values {
			check_geometry_depth(value, depth + 1)?;
		}
	}
	Ok(())
}

/// Registers historical class names with dense group-local identities.
fn register_class(
	context: &mut EncodeContext,
	name: &Text,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<()> {
	check_text(name.len())?;
	if context.class_id(name).is_some() {
		return Ok(());
	}
	let id = context
		.class_names()
		.len()
		.checked_add(1)
		.ok_or_else(|| invalid("changefeed class count overflows"))?;
	if id > MAX_COUNT {
		return Err(invalid("changefeed class dictionary exceeds its cap"));
	}
	admit_dictionary_growth(budget, id, std::mem::size_of::<(Text, u32)>())?;
	budget.admit(256 + name.len() * 2)?;
	context.register_class_name(id as u32, name.clone())
}

/// Encodes a persistable value with the existing canonical representation.
pub(in crate::storage) fn encode(
	value: &Value,
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<Vec<u8>> {
	encode_observed(value, context, budget, None)
}

/// Shares canonical value admission with an optional caller cancellation/work
/// hook.
pub(in crate::storage) fn encode_observed(
	value: &Value,
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
	observe: Option<&dyn Fn(u64) -> FerrosGrynnResult<()>>,
) -> FerrosGrynnResult<Vec<u8>> {
	prepare(value, context, budget, 0, observe)?;
	budget.admit(
		value
			.estimated_heap_bytes()
			.checked_mul(8)
			.ok_or_else(|| invalid("changefeed value encode reservation overflows"))?,
	)?;
	let value = StorageValue::from_runtime(value.clone(), context)?;
	CanonicalCodec::encode(value.as_ref())
}

/// Decodes canonical bytes within the remaining aggregate budget and admits
/// runtime dictionary expansion before converting any object or RecordId.
pub(in crate::storage) fn decode(
	bytes: &[u8],
	context: &EncodeContext,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<Value> {
	let limits = DecodeLimits {
		max_total_allocation: budget.maximum - budget.used,
		..DecodeLimits::default()
	};
	let (value, allocated) = CanonicalCodec::decode_with_allocation(bytes, limits)?;
	budget.admit(allocated)?;
	let decoder = context.decoder();
	budget.admit(value.runtime_allocation_bytes(&decoder)?)?;
	value.into_runtime(&decoder)
}

/// Persists historical names and full graph descriptors in exact slot order.
pub(in crate::storage) fn encode_dictionary(
	context: &EncodeContext,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<Vec<u8>> {
	let mut output = Vec::new();
	write_count(&mut output, context.property_dictionary().len(), budget)?;
	for (_key, name) in context.property_dictionary().iter() {
		append_text(&mut output, name, budget)?;
	}
	write_count(&mut output, context.class_names().len(), budget)?;
	for (_id, name) in context.class_names() {
		append_text(&mut output, name, budget)?;
	}
	write_count(&mut output, context.time_zone_names().len(), budget)?;
	for name in context.time_zone_names() {
		append_text(&mut output, name, budget)?;
	}
	write_count(&mut output, context.graph_references().len(), budget)?;
	for (_slot, graph) in context.graph_references() {
		append(&mut output, &graph.id().get().to_le_bytes(), budget)?;
		let kind = match graph.kind() {
			GraphKind::Graph => 0,
			GraphKind::Tree => 1,
			GraphKind::RootedTree => 2,
			GraphKind::Forest => 3,
			GraphKind::Dag => 4,
			GraphKind::Hypergraph => 5,
			GraphKind::Metagraph => 6,
			GraphKind::Subgraph => 7,
		};
		let mode = match graph.storage_mode() {
			GraphStorageMode::InMemory => 0,
			GraphStorageMode::DiskBacked => 1,
			GraphStorageMode::Hybrid => 2,
		};
		append(&mut output, &[kind, mode, 0, 0, 0, 0, 0, 0], budget)?;
		append_text(&mut output, graph.name(), budget)?;
	}
	Ok(output)
}

/// Restores independent historical dictionaries before any image conversion.
pub(in crate::storage) fn decode_dictionary(
	bytes: &[u8],
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<EncodeContext> {
	let mut decoder = Decoder::new(bytes);
	let mut context = EncodeContext::default();
	let properties = count(&mut decoder, 8)?;
	for ordinal in 0..properties {
		let name = text(&mut decoder, budget)?;
		context.register_property_name((ordinal as u32).into(), name)?;
	}
	let classes = count(&mut decoder, 8)?;
	budget.admit(dictionary_tree_bytes(classes, std::mem::size_of::<(Text, u32)>())?)?;
	for ordinal in 0..classes {
		let name = text(&mut decoder, budget)?;
		context.register_class_name(ordinal as u32 + 1, name)?;
	}
	let zones = count(&mut decoder, 8)?;
	budget.admit(tree_allocation_bytes(zones, std::mem::size_of::<(Text, u32)>())?)?;
	for ordinal in 0..zones {
		let name = text(&mut decoder, budget)?;
		if context.register_time_zone_name(name)? != ordinal as u32 + 1 {
			return Err(invalid("changefeed time-zone dictionary has duplicate names"));
		}
	}
	let graphs = count(&mut decoder, 24)?;
	budget
		.admit(dictionary_tree_bytes(graphs, std::mem::size_of::<(GraphRef, DiskGraphRef)>())?)?;
	for ordinal in 0..graphs {
		let id = GraphId::new(decoder.u64()?)
			.ok_or_else(|| invalid("changefeed historical graph identity is zero"))?;
		let kind = match decoder.u8()? {
			0 => GraphKind::Graph,
			1 => GraphKind::Tree,
			2 => GraphKind::RootedTree,
			3 => GraphKind::Forest,
			4 => GraphKind::Dag,
			5 => GraphKind::Hypergraph,
			6 => GraphKind::Metagraph,
			7 => GraphKind::Subgraph,
			_ => return Err(invalid("changefeed historical graph kind is unknown")),
		};
		let mode = match decoder.u8()? {
			0 => GraphStorageMode::InMemory,
			1 => GraphStorageMode::DiskBacked,
			2 => GraphStorageMode::Hybrid,
			_ => return Err(invalid("changefeed historical graph mode is unknown")),
		};
		decoder.zero(6)?;
		let name = text(&mut decoder, budget)?;
		let graph = GraphRef::new(id, name, kind, mode)?;
		let slot = GraphId::new(ordinal as u64 + 1)
			.ok_or_else(|| invalid("changefeed historical graph slot is zero"))?;
		budget.admit(512)?;
		context.register_historical_graph(graph, DiskGraphRef::new(slot))?;
	}
	decoder.finish()?;
	Ok(context)
}

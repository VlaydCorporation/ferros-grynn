//! Versioned logical graph checkpoints with bounded self-contained
//! dictionaries.

use std::collections::{BTreeMap, BTreeSet};

use rapidhash::v3::rapidhash_v3;

use crate::{
	changefeed::ChangeIdentity,
	datastore::ClassId,
	gql_status::FerrosGrynnResult,
	graph::{AtomKind, EdgeDirection, GraphId, GraphKind, GraphTopologyConstraints},
	live::{
		LiveGraphAtomId,
		LiveGraphEdge,
		LiveGraphEndpoint,
		LiveGraphImage,
		LiveGraphNode,
		LiveGraphScope,
		LiveGraphScopeId,
		LiveGraphSource,
	},
	storage::history_codec::{
		CodecBudget,
		Decoder,
		append,
		append_blob,
		checked_payload,
		invalid,
		logical_values::{self, Work},
		qualified,
		values,
	},
	values::{Object, PropertyKeyId, Text, Value, storage::EncodeContext},
};

/// The fixed versioned FGLG header length, including framing and checksum.
const HEADER: usize = 32;
/// Independent decode work ceiling for attacker-controlled graph counts.
const MAX_NODES: usize = 1_000_000;

/// Maps the complete closed family enum to its normative byte tag.
fn family(kind: GraphKind) -> u8 {
	match kind {
		GraphKind::Graph => 0,
		GraphKind::Tree => 1,
		GraphKind::RootedTree => 2,
		GraphKind::Forest => 3,
		GraphKind::Dag => 4,
		GraphKind::Hypergraph => 5,
		GraphKind::Metagraph => 6,
		GraphKind::Subgraph => 7,
	}
}
/// Rejects unknown durable family tags before restoring a graph.
fn read_family(input: &mut Decoder<'_>) -> FerrosGrynnResult<GraphKind> {
	Ok(match input.u8()? {
		0 => GraphKind::Graph,
		1 => GraphKind::Tree,
		2 => GraphKind::RootedTree,
		3 => GraphKind::Forest,
		4 => GraphKind::Dag,
		5 => GraphKind::Hypergraph,
		6 => GraphKind::Metagraph,
		7 => GraphKind::Subgraph,
		_ => return Err(invalid("graph checkpoint family is unknown")),
	})
}
/// Reads only known orthogonal topology validator bits.
fn constraints(input: &mut Decoder<'_>) -> FerrosGrynnResult<GraphTopologyConstraints> {
	let bits =
		u16::from_le_bytes(input.take(2)?.try_into().map_err(|error| {
			invalid("graph validators are truncated").with_technical_source(error)
		})?);
	GraphTopologyConstraints::from_bits(bits)
		.ok_or_else(|| invalid("graph checkpoint validators are unknown"))
}
/// Rejects noncanonical presence flags.
fn flag(input: &mut Decoder<'_>) -> FerrosGrynnResult<bool> {
	match input.u8()? {
		0 => Ok(false),
		1 => Ok(true),
		_ => Err(invalid("graph checkpoint presence flag is invalid")),
	}
}

/// Writes a stable source scope or bounded canonical derived route.
fn scope(
	output: &mut Vec<u8>,
	value: &LiveGraphScopeId,
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work,
) -> FerrosGrynnResult<()> {
	work.take(0, 1)?;
	match value {
		LiveGraphScopeId::Root => append(output, &[0], budget)?,
		LiveGraphScopeId::Source(id) => {
			append(output, &[1], budget)?;
			append(output, &id.get().to_le_bytes(), budget)?;
		}
		LiveGraphScopeId::Derived(path) => {
			if path.is_empty() || path.len() > 64 {
				return Err(invalid("computed graph scope route is invalid"));
			}
			append(output, &[2], budget)?;
			values::write_count(output, path.len(), budget)?;
			for owner in path {
				work.take(0, 1)?;
				match owner {
					ChangeIdentity::Record(atom) => {
						append(output, &[0], budget)?;
						qualified::encode_atom(output, atom, context, budget)?;
					}
					ChangeIdentity::Occurrence(atom) => {
						append(output, &[1], budget)?;
						qualified::encode_occurrence(output, atom, context, budget)?;
					}
				}
			}
		}
	}
	Ok(())
}

/// Admits and decodes a checked stable scope identity.
fn read_scope(
	input: &mut Decoder<'_>,
	context: &EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work,
) -> FerrosGrynnResult<LiveGraphScopeId> {
	work.take(0, 1)?;
	Ok(match input.u8()? {
		0 => LiveGraphScopeId::Root,
		1 => LiveGraphScopeId::Source(
			GraphId::new(input.u64()?)
				.ok_or_else(|| invalid("graph source scope ID is reserved"))?,
		),
		2 => {
			let count = values::count(input, 9)?;
			if count == 0 || count > 64 {
				return Err(invalid("computed graph route count is invalid"));
			}
			work.take(0, count)?;
			budget.admit(
				count
					.checked_mul(std::mem::size_of::<ChangeIdentity>())
					.ok_or_else(|| invalid("graph route allocation overflows"))?,
			)?;
			let mut route = Vec::new();
			route.try_reserve_exact(count).map_err(|error| {
				invalid("graph route allocation failed").with_technical_source(error)
			})?;
			for _ in 0..count {
				route.push(match input.u8()? {
					0 => ChangeIdentity::Record(qualified::decode_atom(input, context, budget)?),
					1 => ChangeIdentity::Occurrence(qualified::decode_occurrence(
						input, context, budget,
					)?),
					_ => return Err(invalid("computed graph owner tag is unknown")),
				});
			}
			LiveGraphScopeId::Derived(route)
		}
		_ => return Err(invalid("graph scope tag is unknown")),
	})
}

/// Writes a logical scope and stable atom identity, never a local slot.
fn atom_id(
	output: &mut Vec<u8>,
	value: &LiveGraphAtomId,
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work,
) -> FerrosGrynnResult<()> {
	scope(output, &value.scope, context, budget, work)?;
	qualified::encode_atom(output, &value.atom, context, budget)
}
/// Restores the qualified logical atom identity through the checked value
/// dictionary.
fn read_atom_id(
	input: &mut Decoder<'_>,
	context: &EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work,
) -> FerrosGrynnResult<LiveGraphAtomId> {
	Ok(LiveGraphAtomId {
		scope: read_scope(input, context, budget, work)?,
		atom: qualified::decode_atom(input, context, budget)?,
	})
}

/// Writes qualified incidence and the complete ordered containment route.
fn endpoints(
	output: &mut Vec<u8>,
	values: &[LiveGraphEndpoint],
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work,
) -> FerrosGrynnResult<()> {
	values::write_count(output, values.len(), budget)?;
	for value in values {
		atom_id(output, &value.atom, context, budget, work)?;
		if value.owners.len() > 64 {
			return Err(invalid("graph endpoint route exceeds 64"));
		}
		values::write_count(output, value.owners.len(), budget)?;
		for owner in &value.owners {
			atom_id(output, owner, context, budget, work)?;
		}
	}
	Ok(())
}

/// Admits endpoint and owner vectors before consuming their checked identities.
fn read_endpoints(
	input: &mut Decoder<'_>,
	context: &EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work,
) -> FerrosGrynnResult<Vec<LiveGraphEndpoint>> {
	let count = values::count(input, 13)?;
	work.take(0, count)?;
	budget.admit(
		count
			.checked_mul(std::mem::size_of::<LiveGraphEndpoint>())
			.ok_or_else(|| invalid("graph endpoints allocation overflows"))?,
	)?;
	let mut output = Vec::new();
	output.try_reserve_exact(count).map_err(|error| {
		invalid("graph endpoints allocation failed").with_technical_source(error)
	})?;
	for _ in 0..count {
		let atom = read_atom_id(input, context, budget, work)?;
		let count = values::count(input, 9)?;
		if count > 64 {
			return Err(invalid("graph endpoint route exceeds 64"));
		}
		work.take(0, count)?;
		budget.admit(
			count
				.checked_mul(std::mem::size_of::<LiveGraphAtomId>())
				.ok_or_else(|| invalid("graph owners allocation overflows"))?,
		)?;
		let mut owners = Vec::new();
		owners.try_reserve_exact(count).map_err(|error| {
			invalid("graph owners allocation failed").with_technical_source(error)
		})?;
		for _ in 0..count {
			owners.push(read_atom_id(input, context, budget, work)?);
		}
		output.push(LiveGraphEndpoint {
			atom,
			owners,
		});
	}
	Ok(output)
}

/// Writes owned named properties through the logical value codec.
fn object(
	output: &mut Vec<u8>,
	value: &Object,
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work,
) -> FerrosGrynnResult<()> {
	let mut bytes = Vec::new();
	work.take(0, 1)?;
	append(&mut bytes, &[2], budget)?;
	values::write_count(&mut bytes, value.len(), budget)?;
	for (name, value) in value {
		values::append_text(&mut bytes, name, budget)?;
		logical_values::encode(&mut bytes, value, context, budget, work, 1)?;
	}
	append_blob(output, &bytes, budget)
}
/// Restores named properties under the common byte and node budgets.
fn read_object(
	input: &mut Decoder<'_>,
	context: &EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work,
) -> FerrosGrynnResult<Object> {
	let mut blob = Decoder::new(input.blob()?);
	let value = logical_values::decode(&mut blob, context, budget, work, 0)?;
	blob.finish()?;
	match value {
		Value::Object(object) => Ok(object),
		_ => Err(invalid("graph properties are not an owned object")),
	}
}
/// Writes an explicit optional occurrence label.
fn label(
	output: &mut Vec<u8>,
	value: Option<&Text>,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<()> {
	append(output, &[u8::from(value.is_some())], budget)?;
	if let Some(value) = value {
		values::append_text(output, value, budget)?;
	}
	Ok(())
}
/// Restores a checked optional occurrence label.
fn read_label(
	input: &mut Decoder<'_>,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<Option<Text>> {
	if flag(input)? {
		values::text(input, budget).map(Some)
	} else {
		Ok(None)
	}
}
/// Captures the stable source mapping independently of local slot handles.
fn source(
	output: &mut Vec<u8>,
	value: Option<LiveGraphSource>,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<()> {
	append(output, &value.map_or(0, |source| source.class.get()).to_le_bytes(), budget)?;
	append(output, &value.map_or(0, |source| source.revision.get()).to_le_bytes(), budget)
}
/// Restores the stable class incarnation and accepted source snapshot.
fn read_source(input: &mut Decoder<'_>) -> FerrosGrynnResult<Option<LiveGraphSource>> {
	let class = input.u32()?;
	let revision = input.u64()?;
	if class == 0 {
		if revision != 0 {
			return Err(invalid("absent graph source has a revision"));
		}
		Ok(None)
	} else {
		Ok(Some(LiveGraphSource {
			class: ClassId::new(class)
				.ok_or_else(|| invalid("graph source class ID is reserved"))?,
			revision: revision.into(),
		}))
	}
}
/// Borrows the immutable layout bound to a node or edge owner.
fn child(
	output: &mut Vec<u8>,
	value: Option<&LiveGraphScopeId>,
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work,
) -> FerrosGrynnResult<()> {
	append(output, &[u8::from(value.is_some())], budget)?;
	if let Some(value) = value {
		scope(output, value, context, budget, work)?;
	}
	Ok(())
}
/// Restores an optional shared child scope after validating its presence flag.
fn read_child(
	input: &mut Decoder<'_>,
	context: &EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work,
) -> FerrosGrynnResult<Option<LiveGraphScopeId>> {
	if flag(input)? {
		read_scope(input, context, budget, work).map(Some)
	} else {
		Ok(None)
	}
}

/// Writes an independent checkpoint without serializing executable IR or
/// runtime graphs.
pub fn encode(image: &LiveGraphImage, maximum: usize) -> FerrosGrynnResult<Vec<u8>> {
	let mut budget = CodecBudget::new(maximum, 0)?;
	let mut work = Work {
		remaining: MAX_NODES,
	};
	encode_bounded(image, &mut budget, &mut work)
}

/// Shares aggregate scratch and work with its enclosing history envelope.
pub(in crate::storage) fn encode_bounded(
	image: &LiveGraphImage,
	budget: &mut CodecBudget,
	work: &mut Work,
) -> FerrosGrynnResult<Vec<u8>> {
	budget.admit(image.estimated_heap_bytes()?)?;
	let mut context = EncodeContext::default();
	image.validate_structure(&mut |bytes, units| {
		budget.admit(bytes)?;
		work.take(
			0,
			usize::try_from(units).map_err(|error| {
				invalid("graph validation work exceeds usize").with_technical_source(error)
			})?,
		)
	})?;
	let mut body = Vec::new();
	append(&mut body, &[family(image.family)], budget)?;
	append(&mut body, &image.validators.bits().to_le_bytes(), budget)?;
	append(&mut body, &[u8::from(image.root.is_some())], budget)?;
	if let Some(root) = &image.root {
		qualified::encode_atom(&mut body, root, &mut context, budget)?;
	}
	values::write_count(&mut body, image.property_keys.len(), budget)?;
	for (name, key) in &image.property_keys {
		work.take(0, 1)?;
		values::append_text(&mut body, name, budget)?;
		append(&mut body, &key.get().to_le_bytes(), budget)?;
	}
	values::write_count(&mut body, image.scopes.len(), budget)?;
	for (key, graph) in &image.scopes {
		scope(&mut body, key, &mut context, budget, work)?;
		let slot = graph
			.source
			.as_ref()
			.map(|graph| values::graph_slot(&mut context, graph, budget))
			.transpose()?
			.unwrap_or(0);
		append(&mut body, &slot.to_le_bytes(), budget)?;
		append(&mut body, &graph.validators.bits().to_le_bytes(), budget)?;
		object(&mut body, &graph.properties, &mut context, budget, work)?;
		values::write_count(&mut body, graph.nodes.len(), budget)?;
		for (atom, node) in &graph.nodes {
			work.take(0, 1)?;
			qualified::encode_atom(&mut body, atom, &mut context, budget)?;
			label(&mut body, node.label.as_ref(), budget)?;
			object(&mut body, &node.properties, &mut context, budget, work)?;
			source(&mut body, node.source, budget)?;
			child(&mut body, node.child.as_ref(), &mut context, budget, work)?;
		}
		values::write_count(&mut body, graph.edges.len(), budget)?;
		for (atom, edge) in &graph.edges {
			work.take(0, 1)?;
			qualified::encode_atom(&mut body, atom, &mut context, budget)?;
			label(&mut body, edge.label.as_ref(), budget)?;
			object(&mut body, &edge.properties, &mut context, budget, work)?;
			source(&mut body, edge.source, budget)?;
			child(&mut body, edge.child.as_ref(), &mut context, budget, work)?;
			append(
				&mut body,
				&[
					match edge.direction {
						EdgeDirection::Directed => 0,
						EdgeDirection::Bidirectional => 1,
						EdgeDirection::Undirected => 2,
					},
					u8::from(edge.weight.is_some()),
				],
				budget,
			)?;
			if let Some(weight) = edge.weight {
				let bytes = values::encode(&Value::from(weight), &mut context, budget)?;
				append_blob(&mut body, &bytes, budget)?;
			}
			endpoints(&mut body, &edge.inputs, &mut context, budget, work)?;
			endpoints(&mut body, &edge.outputs, &mut context, budget, work)?;
		}
	}
	let dictionary = values::encode_dictionary(&context, budget)?;
	let length = HEADER
		.checked_add(dictionary.len())
		.and_then(|size| size.checked_add(body.len()))
		.ok_or_else(|| invalid("graph checkpoint size overflows"))?;
	checked_payload(length as u64)?;
	let mut output = Vec::new();
	append(&mut output, b"FGLG\x01\x00\x00\x00", budget)?;
	append(&mut output, &(dictionary.len() as u64).to_le_bytes(), budget)?;
	append(&mut output, &(body.len() as u64).to_le_bytes(), budget)?;
	append(&mut output, &0u64.to_le_bytes(), budget)?;
	append(&mut output, &dictionary, budget)?;
	append(&mut output, &body, budget)?;
	let checksum = rapidhash_v3(&output);
	output[24..32].copy_from_slice(&checksum.to_le_bytes());
	Ok(output)
}

/// Validates framing and checksums before allocating dictionaries or graph
/// content.
#[cfg(test)]
pub fn decode(bytes: &[u8], maximum: usize) -> FerrosGrynnResult<LiveGraphImage> {
	let mut budget = CodecBudget::new(maximum, bytes.len())?;
	let mut work = Work {
		remaining: MAX_NODES,
	};
	decode_bounded(bytes, &mut budget, &mut work)
}

/// Shares aggregate allocation and work limits across all checkpoint images.
pub(in crate::storage) fn decode_bounded(
	bytes: &[u8],
	budget: &mut CodecBudget,
	work: &mut Work,
) -> FerrosGrynnResult<LiveGraphImage> {
	checked_payload(bytes.len() as u64)?;
	let mut input = Decoder::new(bytes);
	if input.take(8)? != b"FGLG\x01\x00\x00\x00" {
		return Err(invalid("graph checkpoint marker/version/flags is invalid"));
	}
	let dictionary = usize::try_from(input.u64()?).map_err(|error| {
		invalid("graph dictionary length exceeds usize").with_technical_source(error)
	})?;
	let body = usize::try_from(input.u64()?)
		.map_err(|error| invalid("graph body length exceeds usize").with_technical_source(error))?;
	let checksum = input.u64()?;
	if HEADER.checked_add(dictionary).and_then(|length| length.checked_add(body))
		!= Some(bytes.len())
	{
		return Err(invalid("graph checkpoint lengths are invalid"));
	}
	budget.admit(bytes.len())?;
	let mut checked = bytes.to_vec();
	checked[24..32].fill(0);
	if rapidhash_v3(&checked) != checksum {
		return Err(invalid("graph checkpoint checksum mismatch"));
	}
	drop(checked);
	let context = values::decode_dictionary(input.take(dictionary)?, budget)?;
	let mut data = Decoder::new(input.take(body)?);
	let family = read_family(&mut data)?;
	let validators = constraints(&mut data)?;
	let root = if flag(&mut data)? {
		Some(qualified::decode_atom(&mut data, &context, budget)?)
	} else {
		None
	};
	let count = values::count(&mut data, 12)?;
	work.take(0, count)?;
	let mut property_keys = BTreeMap::new();
	let mut property_ids = BTreeSet::new();
	for _ in 0..count {
		budget.admit(384)?;
		let name = values::text(&mut data, budget)?;
		let key = PropertyKeyId::new(data.u32()?);
		if property_keys.last_key_value().is_some_and(|(last, _)| last >= &name)
			|| !property_ids.insert(key)
		{
			return Err(invalid("graph property keys are not unique/canonical"));
		}
		property_keys.insert(name, key);
	}
	let count = values::count(&mut data, 23)?;
	work.take(0, count)?;
	let mut scopes = BTreeMap::new();
	for _ in 0..count {
		budget.admit(256 + std::mem::size_of::<LiveGraphScope>())?;
		let key = read_scope(&mut data, &context, budget, work)?;
		if scopes.last_key_value().is_some_and(|(last, _)| last >= &key) {
			return Err(invalid("graph scopes are not canonical"));
		}
		let slot = data.u64()?;
		let source = if slot == 0 {
			None
		} else {
			Some(values::graph(&context, slot, budget)?)
		};
		let scope_validators = constraints(&mut data)?;
		let properties = read_object(&mut data, &context, budget, work)?;
		let count = values::count(&mut data, 24)?;
		work.take(0, count)?;
		let mut nodes = BTreeMap::new();
		for _ in 0..count {
			budget.admit(256 + std::mem::size_of::<LiveGraphNode>())?;
			let atom = qualified::decode_atom(&mut data, &context, budget)?;
			if atom.kind() != AtomKind::Node
				|| nodes.last_key_value().is_some_and(|(last, _)| last >= &atom)
			{
				return Err(invalid("graph node identities are not canonical"));
			}
			let label = read_label(&mut data, budget)?;
			let properties = read_object(&mut data, &context, budget, work)?;
			let source = read_source(&mut data)?;
			let child = read_child(&mut data, &context, budget, work)?;
			nodes.insert(atom, LiveGraphNode {
				label,
				properties,
				child,
				source,
			});
		}
		let count = values::count(&mut data, 34)?;
		work.take(0, count)?;
		let mut edges = BTreeMap::new();
		for _ in 0..count {
			budget.admit(256 + std::mem::size_of::<LiveGraphEdge>())?;
			let atom = qualified::decode_atom(&mut data, &context, budget)?;
			if atom.kind() != AtomKind::Edge
				|| edges.last_key_value().is_some_and(|(last, _)| last >= &atom)
			{
				return Err(invalid("graph edge identities are not canonical"));
			}
			let label = read_label(&mut data, budget)?;
			let properties = read_object(&mut data, &context, budget, work)?;
			let source = read_source(&mut data)?;
			let child = read_child(&mut data, &context, budget, work)?;
			let direction = match data.u8()? {
				0 => EdgeDirection::Directed,
				1 => EdgeDirection::Bidirectional,
				2 => EdgeDirection::Undirected,
				_ => return Err(invalid("graph edge direction is unknown")),
			};
			let weight = if flag(&mut data)? {
				match values::decode(data.blob()?, &context, budget)? {
					Value::Number(weight) => Some(weight),
					_ => return Err(invalid("graph weight is not numeric")),
				}
			} else {
				None
			};
			let inputs = read_endpoints(&mut data, &context, budget, work)?;
			let outputs = read_endpoints(&mut data, &context, budget, work)?;
			edges.insert(atom, LiveGraphEdge {
				label,
				properties,
				direction,
				inputs,
				outputs,
				weight,
				child,
				source,
			});
		}
		scopes.insert(key, LiveGraphScope {
			source,
			validators: scope_validators,
			properties,
			nodes,
			edges,
		});
	}
	data.finish()?;
	input.finish()?;
	let image = LiveGraphImage {
		family,
		validators,
		root,
		scopes,
		property_keys,
	};
	image.validate_structure(&mut |bytes, units| {
		budget.admit(bytes)?;
		work.take(
			0,
			usize::try_from(units).map_err(|error| {
				invalid("graph validation work exceeds usize").with_technical_source(error)
			})?,
		)
	})?;
	Ok(image)
}

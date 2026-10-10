//! Explicit versioned logical LIVE recipes; executable typed IR is never
//! stored.

#![allow(
	clippy::redundant_pub_crate,
	reason = "Logical recipes cross private storage/datastore boundaries"
)]

use super::history_codec::{
	CodecBudget,
	Decoder,
	append,
	append_blob,
	checked_payload,
	invalid,
	values,
};
use crate::{
	datastore::ClassId,
	gql_status::FerrosGrynnResult,
	live::{
		LiveBinaryOperator,
		LiveBindingId,
		LiveExpr,
		LiveField,
		LiveOutputMode,
		LivePath,
		LiveProjection,
		LiveQuery,
		LiveSource,
	},
	values::{Object, Value, storage::EncodeContext},
};

/// Maximum aggregate logical nodes and collection entries in one descriptor.
const MAX_NODES: usize = 100_000;
/// Fixed header, dictionary and body length fields included.
const HEADER_BYTES: usize = 24;

/// Comparison recipes and exact optional window expressions.
mod window;

/// Checked ordered JOIN declarations reuse the source and expression codecs.
mod join;

/// Restorable recipe before catalog identity/version and dependency resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
	clippy::large_enum_variant,
	reason = "One short-lived codec input is moved into the registry; boxing adds a separate \
	          allocation"
)]
pub(crate) enum LiveDescriptor {
	/// One reactive read-only query.
	Query(LiveQuery),
	/// One reactive value with ordinary parameter captures.
	Binding {
		/// Logical expression; a value replacement is a literal expression.
		expression: LiveExpr,
		/// Frozen lexical inputs, separate from reactive binding identities.
		parameters: Object,
	},
}

/// Borrowed canonical recipe used by catalog encoding and read-only planning.
#[derive(Clone, Copy)]
pub(crate) enum LiveDescriptorRef<'recipe> {
	/// One reactive query, without cloning captures or expression children.
	Query(&'recipe LiveQuery),
	/// One reactive expression with frozen ordinary inputs.
	Binding {
		/// Logical expression; executable typed IR remains private to GQL.
		expression: &'recipe LiveExpr,
		/// Owned elsewhere; encoding never retains this borrow.
		parameters: &'recipe Object,
	},
}

impl LiveDescriptor {
	/// Borrows the exact variant without changing canonical bytes.
	pub(crate) fn as_ref(&self) -> LiveDescriptorRef<'_> {
		match self {
			Self::Query(query) => LiveDescriptorRef::Query(query),
			Self::Binding {
				expression,
				parameters,
			} => LiveDescriptorRef::Binding {
				expression,
				parameters,
			},
		}
	}
}

/// Bounds recursion and total nodes independently of byte admission.
struct Work<'caller> {
	/// Optional caller work/cancellation admission before traversal.
	observe: Option<&'caller dyn Fn(u64) -> FerrosGrynnResult<()>>,
	/// Nodes and collection entries remaining in this descriptor.
	remaining: usize,
}

impl Work<'_> {
	/// Checks depth before recursion and count before allocation.
	fn take(&mut self, depth: usize, count: usize) -> FerrosGrynnResult<()> {
		if let Some(observe) = self.observe {
			observe(count as u64)?;
		}
		if depth > 64 {
			return Err(invalid("LIVE descriptor nesting exceeds 64"));
		}
		self.remaining = self
			.remaining
			.checked_sub(count)
			.ok_or_else(|| invalid("LIVE descriptor node count exceeds its cap"))?;
		Ok(())
	}
}

/// Encodes one logical recipe with independent historical value dictionaries.
pub(crate) fn encode(descriptor: &LiveDescriptor, maximum: usize) -> FerrosGrynnResult<Vec<u8>> {
	encode_ref(descriptor.as_ref(), maximum)
}

/// Encodes a bounded borrowed recipe without allocating an owned input copy.
pub(crate) fn encode_ref(
	descriptor: LiveDescriptorRef<'_>,
	maximum: usize,
) -> FerrosGrynnResult<Vec<u8>> {
	let mut budget = CodecBudget::new(maximum, 0)?;
	encode_ref_bounded(descriptor, &mut budget)
}

/// Encodes within caller work/cancellation admission using the same canonical
/// traversal.
pub(crate) fn encode_ref_controlled(
	descriptor: LiveDescriptorRef<'_>,
	maximum: usize,
	observe: &dyn Fn(u64) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<Vec<u8>> {
	let mut budget = CodecBudget::new(maximum, 0)?;
	encode_ref_observed(descriptor, &mut budget, Some(observe))
}

/// Encodes a borrowed evaluated value without cloning its potentially large
/// payload.
pub(crate) fn encode_binding_value(value: &Value, maximum: usize) -> FerrosGrynnResult<Vec<u8>> {
	let mut budget = CodecBudget::new(maximum, 0)?;
	let mut context = EncodeContext::default();
	let mut body = Vec::new();
	append(&mut body, &[0], &mut budget)?;
	encode_value(&mut body, value, &mut context, &mut budget, None)?;
	encode_parameters(&mut body, &Object::new(), &mut context, &mut budget, &mut Work {
		observe: None,
		remaining: MAX_NODES - 1,
	})?;
	encode_frame(&body, &context, 1, &mut budget)
}

/// Shares aggregate admission with canonical decode validation.
fn encode_bounded(
	descriptor: &LiveDescriptor,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<Vec<u8>> {
	encode_ref_bounded(descriptor.as_ref(), budget)
}

/// Shared canonical framing and validation for owned and borrowed inputs.
fn encode_ref_bounded(
	descriptor: LiveDescriptorRef<'_>,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<Vec<u8>> {
	encode_ref_observed(descriptor, budget, None)
}

/// Keeps byte framing identical while admitting caller traversal before growth.
fn encode_ref_observed(
	descriptor: LiveDescriptorRef<'_>,
	budget: &mut CodecBudget,
	observe: Option<&dyn Fn(u64) -> FerrosGrynnResult<()>>,
) -> FerrosGrynnResult<Vec<u8>> {
	let mut context = EncodeContext::default();
	let mut body = Vec::new();
	let mut work = Work {
		observe,
		remaining: MAX_NODES,
	};
	let kind = match descriptor {
		LiveDescriptorRef::Query(query) => {
			encode_query(&mut body, query, &mut context, budget, &mut work, 0)?;
			0
		}
		LiveDescriptorRef::Binding {
			expression,
			parameters,
		} => {
			encode_expr(&mut body, expression, &mut context, budget, &mut work, 0)?;
			encode_parameters(&mut body, parameters, &mut context, budget, &mut work)?;
			1
		}
	};
	encode_frame(&body, &context, kind, budget)
}

/// One authoritative framing path for logical definitions and evaluated values.
fn encode_frame(
	body: &[u8],
	context: &EncodeContext,
	kind: u8,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<Vec<u8>> {
	let dictionary = values::encode_dictionary(context, budget)?;
	let mut output = Vec::new();
	append(&mut output, b"FGLD", budget)?;
	append(&mut output, &[1, 0, kind, 0], budget)?;
	append(&mut output, &(dictionary.len() as u64).to_le_bytes(), budget)?;
	append(&mut output, &(body.len() as u64).to_le_bytes(), budget)?;
	append(&mut output, &dictionary, budget)?;
	append(&mut output, body, budget)?;
	Ok(output)
}

/// Checks framing before dictionary allocation and reconstructs an owned
/// recipe.
pub(crate) fn decode(bytes: &[u8], maximum: usize) -> FerrosGrynnResult<LiveDescriptor> {
	checked_payload(bytes.len() as u64)?;
	let mut budget = CodecBudget::new(maximum, bytes.len())?;
	decode_bounded(bytes, &mut budget)
}

/// Restores a recipe within an enclosing catalog's aggregate admission.
pub(in crate::storage) fn decode_bounded(
	bytes: &[u8],
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<LiveDescriptor> {
	checked_payload(bytes.len() as u64)?;
	let mut decoder = Decoder::new(bytes);
	if decoder.take(4)? != b"FGLD" || decoder.take(2)? != [1, 0] {
		return Err(invalid("LIVE descriptor magic/version is invalid"));
	}
	let kind = decoder.u8()?;
	decoder.zero(1)?;
	if kind > 1 {
		return Err(invalid("LIVE descriptor kind is unknown"));
	}
	let dictionary_len = checked_payload(decoder.u64()?)?;
	let body_len = checked_payload(decoder.u64()?)?;
	if dictionary_len.checked_add(body_len).and_then(|len| len.checked_add(HEADER_BYTES))
		!= Some(bytes.len())
	{
		return Err(invalid("LIVE descriptor section lengths are inconsistent"));
	}
	let context = values::decode_dictionary(decoder.take(dictionary_len)?, budget)?;
	let mut body = Decoder::new(decoder.take(body_len)?);
	decoder.finish()?;
	let mut work = Work {
		observe: None,
		remaining: MAX_NODES,
	};
	let descriptor = match kind {
		0 => LiveDescriptor::Query(decode_query(&mut body, &context, budget, &mut work, 0)?),
		1 => LiveDescriptor::Binding {
			expression: decode_expr(&mut body, &context, budget, &mut work, 0)?,
			parameters: decode_parameters(&mut body, &context, budget, &mut work)?,
		},
		_ => return Err(invalid("LIVE descriptor kind is unknown")),
	};
	body.finish()?;
	if encode_bounded(&descriptor, budget)?.as_slice() != bytes {
		return Err(invalid("LIVE descriptor bytes are not canonical"));
	}
	Ok(descriptor)
}

/// Encodes a literal through the authoritative canonical StorageValue codec.
fn encode_value(
	output: &mut Vec<u8>,
	value: &Value,
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
	observe: Option<&dyn Fn(u64) -> FerrosGrynnResult<()>>,
) -> FerrosGrynnResult<()> {
	let bytes = values::encode_observed(value, context, budget, observe)?;
	append_blob(output, &bytes, budget)
}

/// Admits text hashing/copying through the caller before output grows.
fn encode_text(
	output: &mut Vec<u8>,
	text: &crate::values::Text,
	budget: &mut CodecBudget,
	work: &Work<'_>,
) -> FerrosGrynnResult<()> {
	if let Some(observe) = work.observe {
		observe(text.len() as u64)?;
	}
	values::append_text(output, text, budget)
}

/// Writes one checked logical path, preserving segment boundaries.
fn encode_path(
	output: &mut Vec<u8>,
	path: &LivePath,
	budget: &mut CodecBudget,
	work: &mut Work<'_>,
) -> FerrosGrynnResult<()> {
	work.take(0, path.segments().len())?;
	values::write_count(output, path.segments().len(), budget)?;
	for name in path.segments() {
		encode_text(output, name, budget, work)?;
	}
	Ok(())
}

/// Restores a checked path after count and scratch admission.
fn decode_path(
	decoder: &mut Decoder<'_>,
	budget: &mut CodecBudget,
	work: &mut Work<'_>,
) -> FerrosGrynnResult<LivePath> {
	let count = values::count(decoder, 8)?;
	if count == 0 || count > 64 {
		return Err(invalid("LIVE path count is invalid"));
	}
	work.take(0, count)?;
	budget.admit(count * std::mem::size_of::<crate::values::Text>())?;
	let mut segments = Vec::new();
	segments.try_reserve_exact(count).map_err(|_error| invalid("LIVE path allocation failed"))?;
	for _ in 0..count {
		segments.push(values::text(decoder, budget)?);
	}
	LivePath::new(segments)
}

/// Writes ordinary parameter captures in canonical B-tree name order.
fn encode_parameters(
	output: &mut Vec<u8>,
	parameters: &Object,
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work<'_>,
) -> FerrosGrynnResult<()> {
	work.take(0, parameters.len())?;
	values::write_count(output, parameters.len(), budget)?;
	for (name, value) in parameters {
		if name.is_empty() {
			return Err(invalid("LIVE parameter name is empty"));
		}
		encode_text(output, name, budget, work)?;
		encode_value(output, value, context, budget, work.observe)?;
	}
	Ok(())
}

/// Rejects duplicate or unordered parameter names instead of overwriting them.
fn decode_parameters(
	decoder: &mut Decoder<'_>,
	context: &EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work<'_>,
) -> FerrosGrynnResult<Object> {
	let count = values::count(decoder, 16)?;
	work.take(0, count)?;
	budget.admit(crate::values::storage::tree_allocation_bytes(
		count,
		std::mem::size_of::<(crate::values::Text, Value)>(),
	)?)?;
	let mut parameters = Object::new();
	let mut previous = None;
	for _ in 0..count {
		let name = values::text(decoder, budget)?;
		if name.is_empty() || previous.as_ref().is_some_and(|last| last >= &name) {
			return Err(invalid("LIVE parameter names are not strictly ordered"));
		}
		let value = values::decode(decoder.blob()?, context, budget)?;
		budget.admit(name.len())?;
		previous = Some(name.clone());
		parameters.insert(name, value);
	}
	Ok(parameters)
}

/// Writes a source without adding query work or changing child expression
/// depth.
fn encode_source(
	output: &mut Vec<u8>,
	source: &LiveSource,
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work<'_>,
	depth: usize,
) -> FerrosGrynnResult<()> {
	match source {
		LiveSource::Class(class) => {
			append(output, &[0], budget)?;
			append(output, &class.get().to_le_bytes(), budget)?;
		}
		LiveSource::Record(record)
		| LiveSource::GraphRecord {
			record,
			..
		} => {
			if let LiveSource::GraphRecord {
				graph,
				..
			} = source
			{
				append(output, &[4], budget)?;
				let slot = values::graph_slot(context, graph, budget)?;
				append(output, &slot.to_le_bytes(), budget)?;
			} else {
				append(output, &[1], budget)?;
			}
			values::prepare_record(record, context, budget, depth + 1, work.observe)?;
			budget.admit(
				record
					.estimated_heap_bytes()
					.checked_add(std::mem::size_of_val(record))
					.ok_or_else(|| invalid("LIVE record reservation overflows"))?,
			)?;
			encode_value(
				output,
				&Value::Record(Box::new(record.clone())),
				context,
				budget,
				work.observe,
			)?;
		}
		LiveSource::Graph(graph)
		| LiveSource::GraphClass {
			graph,
			..
		} => {
			append(
				output,
				&[if matches!(source, LiveSource::Graph(_)) {
					2
				} else {
					3
				}],
				budget,
			)?;
			let slot = values::graph_slot(context, graph, budget)?;
			append(output, &slot.to_le_bytes(), budget)?;
			if let LiveSource::GraphClass {
				class,
				..
			} = source
			{
				append(output, &class.get().to_le_bytes(), budget)?;
			}
		}
		LiveSource::GraphExpr(graph) => {
			append(output, &[7], budget)?;
			encode_expr(output, graph, context, budget, work, depth + 1)?;
		}
		LiveSource::GraphClassExpr {
			graph,
			class,
		} => {
			append(output, &[5], budget)?;
			append(output, &class.get().to_le_bytes(), budget)?;
			encode_expr(output, graph, context, budget, work, depth + 1)?;
		}
		LiveSource::GraphRecordExpr {
			graph,
			record,
		} => {
			append(output, &[6], budget)?;
			encode_expr(output, graph, context, budget, work, depth + 1)?;
			values::prepare_record(record, context, budget, depth + 1, work.observe)?;
			budget.admit(
				record
					.estimated_heap_bytes()
					.checked_add(std::mem::size_of_val(record))
					.ok_or_else(|| invalid("LIVE record reservation overflows"))?,
			)?;
			encode_value(
				output,
				&Value::Record(Box::new(record.clone())),
				context,
				budget,
				work.observe,
			)?;
		}
	}
	Ok(())
}

/// Writes logical query fields; type/effect/name resolution remains in GQL.
fn encode_query(
	output: &mut Vec<u8>,
	query: &LiveQuery,
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work<'_>,
	depth: usize,
) -> FerrosGrynnResult<()> {
	work.take(depth, 1)?;
	encode_source(output, &query.source, context, budget, work, depth)?;
	append(output, &[u8::from(query.filter.is_some())], budget)?;
	if let Some(filter) = &query.filter {
		encode_expr(output, filter, context, budget, work, depth + 1)?;
	}
	match &query.group {
		None => append(output, &[0], budget)?,
		Some(keys) if keys.is_empty() => append(output, &[1], budget)?,
		Some(keys) => {
			work.take(depth, keys.len())?;
			append(output, &[2], budget)?;
			values::write_count(output, keys.len(), budget)?;
			for key in keys {
				encode_expr(output, key, context, budget, work, depth + 1)?;
			}
		}
	}
	match &query.projection {
		LiveProjection::Records => append(output, &[0], budget)?,
		LiveProjection::Fields(fields) => {
			if fields.is_empty() {
				return Err(invalid("LIVE fields projection is empty"));
			}
			work.take(depth, fields.len())?;
			append(output, &[1], budget)?;
			values::write_count(output, fields.len(), budget)?;
			for field in fields {
				encode_expr(output, &field.expression, context, budget, work, depth + 1)?;
				append(output, &[u8::from(field.alias.is_some())], budget)?;
				if let Some(alias) = &field.alias {
					encode_path(output, alias, budget, work)?;
				}
			}
		}
		LiveProjection::Value(expression) => {
			append(output, &[2], budget)?;
			encode_expr(output, expression, context, budget, work, depth + 1)?;
		}
	}
	window::encode(output, query, context, budget, work, depth)?;
	for paths in [&query.omit, &query.fetch] {
		work.take(depth, paths.len())?;
		values::write_count(output, paths.len(), budget)?;
		for path in paths {
			encode_path(output, path, budget, work)?;
		}
	}
	encode_parameters(output, &query.parameters, context, budget, work)?;
	append(
		output,
		&[(match query.output {
			LiveOutputMode::Full => 0,
			LiveOutputMode::Diff => 1,
			LiveOutputMode::Patch => 2,
		}) | (u8::from(query.only) << 2)
			| (u8::from(!query.split.is_empty()) << 3)
			| (u8::from(query.alias.is_some()) << 4)
			| (u8::from(!query.joins.is_empty()) << 5)],
		budget,
	)?;
	if !query.split.is_empty() {
		work.take(depth, query.split.len())?;
		values::write_count(output, query.split.len(), budget)?;
		for value in &query.split {
			encode_expr(output, value, context, budget, work, depth + 1)?;
		}
	}
	if let Some(alias) = &query.alias {
		LiveQuery::validate_alias(alias.as_str())
			.map_err(|error| invalid("LIVE source alias is malformed").with_cause(error))?;
		work.take(depth, 1)?;
		encode_text(output, alias, budget, work)?;
	}
	if !query.joins.is_empty() {
		join::encode(output, &query.joins, context, budget, work, depth)?;
	}
	Ok(())
}

/// Restores source identities with the enclosing query's admission and depth.
fn decode_source(
	decoder: &mut Decoder<'_>,
	context: &EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work<'_>,
	depth: usize,
) -> FerrosGrynnResult<LiveSource> {
	Ok(match decoder.u8()? {
		0 => LiveSource::Class(class(decoder)?),
		tag @ (1 | 4) => {
			let graph = if tag == 4 {
				Some(values::graph(context, decoder.u64()?, budget)?)
			} else {
				None
			};
			let Value::Record(record) = values::decode(decoder.blob()?, context, budget)? else {
				return Err(invalid("LIVE record source has incorrect value type"));
			};
			if let Some(graph) = graph {
				LiveSource::GraphRecord {
					graph,
					record: *record,
				}
			} else {
				LiveSource::Record(*record)
			}
		}
		2 => LiveSource::Graph(values::graph(context, decoder.u64()?, budget)?),
		3 => LiveSource::GraphClass {
			graph: values::graph(context, decoder.u64()?, budget)?,
			class: class(decoder)?,
		},
		5 => LiveSource::GraphClassExpr {
			class: class(decoder)?,
			graph: Box::new(decode_expr(decoder, context, budget, work, depth + 1)?),
		},
		6 => {
			let graph = Box::new(decode_expr(decoder, context, budget, work, depth + 1)?);
			let Value::Record(record) = values::decode(decoder.blob()?, context, budget)? else {
				return Err(invalid("LIVE expression record source has incorrect value type"));
			};
			LiveSource::GraphRecordExpr {
				graph,
				record: *record,
			}
		}
		7 => {
			LiveSource::GraphExpr(Box::new(decode_expr(decoder, context, budget, work, depth + 1)?))
		}
		_ => return Err(invalid("LIVE source tag is unknown")),
	})
}

/// Restores a query recipe with checked ordinal-free source identities.
fn decode_query(
	decoder: &mut Decoder<'_>,
	context: &EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work<'_>,
	depth: usize,
) -> FerrosGrynnResult<LiveQuery> {
	work.take(depth, 1)?;
	budget.admit(std::mem::size_of::<LiveQuery>())?;
	let source = decode_source(decoder, context, budget, work, depth)?;
	let filter = match decoder.u8()? {
		0 => None,
		1 => Some(decode_expr(decoder, context, budget, work, depth + 1)?),
		_ => return Err(invalid("LIVE filter flag is invalid")),
	};
	let group = match decoder.u8()? {
		0 => None,
		1 => Some(Vec::new()),
		2 => {
			let count = values::count(decoder, 1)?;
			if count == 0 {
				return Err(invalid("LIVE GROUP BY has no key"));
			}
			work.take(depth, count)?;
			budget.admit(
				count
					.checked_mul(std::mem::size_of::<LiveExpr>())
					.ok_or_else(|| invalid("LIVE group size overflows"))?,
			)?;
			let mut keys = Vec::new();
			keys.try_reserve_exact(count)
				.map_err(|_error| invalid("LIVE group allocation failed"))?;
			for _ in 0..count {
				keys.push(decode_expr(decoder, context, budget, work, depth + 1)?);
			}
			Some(keys)
		}
		_ => return Err(invalid("LIVE group tag is unknown")),
	};
	let projection = match decoder.u8()? {
		0 => LiveProjection::Records,
		1 => {
			let count = values::count(decoder, 2)?;
			if count == 0 {
				return Err(invalid("LIVE fields projection is empty"));
			}
			work.take(depth, count)?;
			budget.admit(count * std::mem::size_of::<LiveField>())?;
			let mut fields = Vec::new();
			fields
				.try_reserve_exact(count)
				.map_err(|_error| invalid("LIVE fields allocation failed"))?;
			for _ in 0..count {
				let expression = decode_expr(decoder, context, budget, work, depth + 1)?;
				let alias = match decoder.u8()? {
					0 => None,
					1 => Some(decode_path(decoder, budget, work)?),
					_ => return Err(invalid("LIVE alias flag is invalid")),
				};
				fields.push(LiveField {
					expression,
					alias,
				});
			}
			LiveProjection::Fields(fields)
		}
		2 => LiveProjection::Value(decode_expr(decoder, context, budget, work, depth + 1)?),
		_ => return Err(invalid("LIVE projection tag is unknown")),
	};
	let window = window::decode(decoder, context, budget, work, depth)?;
	let omit = decode_paths(decoder, budget, work, depth)?;
	let fetch = decode_paths(decoder, budget, work, depth)?;
	let parameters = decode_parameters(decoder, context, budget, work)?;
	let flags = decoder.u8()?;
	if flags & !63 != 0 {
		return Err(invalid("LIVE query flags contain reserved bits"));
	}
	let output = match flags & 3 {
		0 => LiveOutputMode::Full,
		1 => LiveOutputMode::Diff,
		2 => LiveOutputMode::Patch,
		_ => return Err(invalid("LIVE output tag is unknown")),
	};
	let split = if flags & 8 != 0 {
		let count = values::count(decoder, 2)?;
		if count == 0 {
			return Err(invalid("LIVE SPLIT tail must contain an operand"));
		}
		work.take(depth, count)?;
		budget.admit(
			count
				.checked_mul(std::mem::size_of::<LiveExpr>())
				.ok_or_else(|| invalid("LIVE SPLIT allocation overflows"))?,
		)?;
		let mut split = Vec::new();
		split.try_reserve_exact(count).map_err(|error| {
			invalid("LIVE SPLIT allocation failed").with_technical_source(error)
		})?;
		for _ in 0..count {
			split.push(decode_expr(decoder, context, budget, work, depth + 1)?);
		}
		split
	} else {
		Vec::new()
	};
	let alias = if flags & 16 != 0 {
		work.take(depth, 1)?;
		let alias = values::text(decoder, budget)?;
		LiveQuery::validate_alias(alias.as_str())
			.map_err(|error| invalid("LIVE source alias is malformed").with_cause(error))?;
		Some(alias)
	} else {
		None
	};
	let joins = if flags & 32 != 0 {
		join::decode(decoder, context, budget, work, depth)?
	} else {
		Vec::new()
	};
	Ok(LiveQuery {
		source,
		alias,
		joins,
		filter,
		split,
		group,
		order: window.order,
		limit: window.limit,
		start_offset: window.start_offset,
		only: flags & 4 != 0,
		projection,
		omit,
		fetch,
		parameters,
		output,
	})
}

/// Restores ordered output paths after checked count and scratch admission.
fn decode_paths(
	decoder: &mut Decoder<'_>,
	budget: &mut CodecBudget,
	work: &mut Work<'_>,
	depth: usize,
) -> FerrosGrynnResult<Vec<LivePath>> {
	let count = values::count(decoder, 12)?;
	work.take(depth, count)?;
	budget.admit(
		count
			.checked_mul(std::mem::size_of::<LivePath>())
			.ok_or_else(|| invalid("LIVE output paths size overflows"))?,
	)?;
	let mut paths = Vec::new();
	paths
		.try_reserve_exact(count)
		.map_err(|_error| invalid("LIVE output paths allocation failed"))?;
	for _ in 0..count {
		paths.push(decode_path(decoder, budget, work)?);
	}
	Ok(paths)
}

/// Rejects reserved class identities from persisted source fields.
fn class(decoder: &mut Decoder<'_>) -> FerrosGrynnResult<ClassId> {
	ClassId::new(decoder.u32()?).ok_or_else(|| invalid("LIVE source class identity is reserved"))
}

/// Writes one expression with a closed, versioned tag set and bounded
/// recursion.
fn encode_expr(
	output: &mut Vec<u8>,
	expression: &LiveExpr,
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work<'_>,
	depth: usize,
) -> FerrosGrynnResult<()> {
	work.take(depth, 1)?;
	match expression {
		LiveExpr::Range {
			start,
			end,
			inclusive_end,
		} => {
			let flags = u8::from(start.is_some())
				| (u8::from(end.is_some()) << 1)
				| (u8::from(*inclusive_end) << 2);
			append(output, &[18, flags], budget)?;
			for child in start.iter().chain(end.iter()) {
				encode_expr(output, child, context, budget, work, depth + 1)?;
			}
		}
		LiveExpr::Cast {
			target,
			value,
		} => {
			crate::live::admit_cast_type_name(
				target.as_str().as_bytes(),
				1,
				&mut |bytes, units| {
					budget.admit(bytes)?;
					work.take(
						depth,
						usize::try_from(units)
							.map_err(|_error| invalid("LIVE cast work overflows"))?,
					)?;
					Ok(())
				},
			)?;
			append(output, &[17], budget)?;
			encode_text(output, target, budget, work)?;
			encode_expr(output, value, context, budget, work, depth + 1)?;
		}
		LiveExpr::Literal(value) => {
			append(output, &[0], budget)?;
			encode_value(output, value, context, budget, work.observe)?;
		}
		LiveExpr::Field(path) => {
			append(output, &[1], budget)?;
			encode_path(output, path, budget, work)?;
		}
		LiveExpr::Parameter(name) => {
			if name.is_empty() {
				return Err(invalid("LIVE parameter name is empty"));
			}
			append(output, &[2], budget)?;
			encode_text(output, name, budget, work)?;
		}
		LiveExpr::Binding(id) => {
			append(output, &[3], budget)?;
			append(output, id.as_uuid().as_inner().as_bytes(), budget)?;
		}
		LiveExpr::Not(child) => {
			append(output, &[4], budget)?;
			encode_expr(output, child, context, budget, work, depth + 1)?;
		}
		LiveExpr::Binary {
			left,
			op,
			right,
		} => {
			append(output, &[5, operator_tag(*op)], budget)?;
			encode_expr(output, left, context, budget, work, depth + 1)?;
			encode_expr(output, right, context, budget, work, depth + 1)?;
		}
		LiveExpr::Call {
			name,
			arguments,
		} => {
			if name.is_empty() {
				return Err(invalid("LIVE function name is empty"));
			}
			work.take(depth, arguments.len())?;
			append(output, &[6], budget)?;
			encode_text(output, name, budget, work)?;
			values::write_count(output, arguments.len(), budget)?;
			for argument in arguments {
				encode_expr(output, argument, context, budget, work, depth + 1)?;
			}
		}
		LiveExpr::CallWithOptions {
			name,
			type_argument,
			distinct,
			arguments,
		} => {
			if name.is_empty() {
				return Err(invalid("LIVE function name is empty"));
			}
			work.take(depth, arguments.len())?;
			append(output, &[19], budget)?;
			encode_text(output, name, budget, work)?;
			append(
				output,
				&[u8::from(type_argument.is_some()) | (u8::from(*distinct) << 1)],
				budget,
			)?;
			if let Some(target) = type_argument {
				crate::live::admit_cast_type_name(
					target.as_str().as_bytes(),
					1,
					&mut |bytes, units| {
						budget.admit(bytes)?;
						work.take(
							depth,
							usize::try_from(units)
								.map_err(|_error| invalid("LIVE call type work overflows"))?,
						)
					},
				)?;
				encode_text(output, target, budget, work)?;
			}
			values::write_count(output, arguments.len(), budget)?;
			for (name, value) in arguments {
				append(output, &[u8::from(name.is_some())], budget)?;
				if let Some(name) = name {
					if name.is_empty() {
						return Err(invalid("LIVE argument name is empty"));
					}
					encode_text(output, name, budget, work)?;
				}
				encode_expr(output, value, context, budget, work, depth + 1)?;
			}
		}
		LiveExpr::Select(query) => {
			append(output, &[7], budget)?;
			encode_query(output, query, context, budget, work, depth + 1)?;
		}
		LiveExpr::Array(values) | LiveExpr::Set(values) => {
			work.take(depth, values.len())?;
			append(
				output,
				&[if matches!(expression, LiveExpr::Set(_)) {
					15
				} else {
					8
				}],
				budget,
			)?;
			values::write_count(output, values.len(), budget)?;
			for value in values {
				encode_expr(output, value, context, budget, work, depth + 1)?;
			}
		}
		LiveExpr::Object(fields) => {
			work.take(depth, fields.len())?;
			append(output, &[9], budget)?;
			values::write_count(output, fields.len(), budget)?;
			for (name, value) in fields {
				encode_text(output, name, budget, work)?;
				encode_expr(output, value, context, budget, work, depth + 1)?;
			}
		}
		LiveExpr::Negate(child) | LiveExpr::Positive(child) => {
			let tag = if matches!(expression, LiveExpr::Negate(_)) {
				10
			} else {
				11
			};
			append(output, &[tag], budget)?;
			encode_expr(output, child, context, budget, work, depth + 1)?;
		}
		LiveExpr::Access {
			base,
			field,
			null_safe,
		} => {
			append(output, &[12, u8::from(*null_safe)], budget)?;
			encode_text(output, field, budget, work)?;
			encode_expr(output, base, context, budget, work, depth + 1)?;
		}
		LiveExpr::Index {
			base,
			index,
		} => {
			append(output, &[13], budget)?;
			encode_expr(output, base, context, budget, work, depth + 1)?;
			encode_expr(output, index, context, budget, work, depth + 1)?;
		}
		LiveExpr::Conditional {
			condition,
			then_value,
			else_value,
		} => {
			append(output, &[14], budget)?;
			for child in [condition, then_value, else_value] {
				encode_expr(output, child, context, budget, work, depth + 1)?;
			}
		}
		LiveExpr::Case {
			operand,
			branches,
			else_value,
		} => {
			if branches.is_empty() {
				return Err(invalid("LIVE CASE requires a branch"));
			}
			work.take(
				depth,
				branches
					.len()
					.checked_mul(2)
					.ok_or_else(|| invalid("LIVE CASE count overflows"))?,
			)?;
			append(output, &[16, u8::from(operand.is_some())], budget)?;
			if let Some(value) = operand {
				encode_expr(output, value, context, budget, work, depth + 1)?;
			}
			values::write_count(output, branches.len(), budget)?;
			for (condition, value) in branches {
				encode_expr(output, condition, context, budget, work, depth + 1)?;
				encode_expr(output, value, context, budget, work, depth + 1)?;
			}
			append(output, &[u8::from(else_value.is_some())], budget)?;
			if let Some(value) = else_value {
				encode_expr(output, value, context, budget, work, depth + 1)?;
			}
		}
	}
	Ok(())
}

/// Restores one expression after aggregate allocation/work admission.
fn decode_expr(
	decoder: &mut Decoder<'_>,
	context: &EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work<'_>,
	depth: usize,
) -> FerrosGrynnResult<LiveExpr> {
	work.take(depth, 1)?;
	budget.admit(std::mem::size_of::<LiveExpr>())?;
	Ok(match decoder.u8()? {
		19 => {
			let name = values::text(decoder, budget)?;
			if name.is_empty() {
				return Err(invalid("LIVE function name is empty"));
			}
			let flags = decoder.u8()?;
			if flags & !3 != 0 {
				return Err(invalid("LIVE call flags contain reserved bits"));
			}
			let type_argument = if flags & 1 != 0 {
				let target =
					crate::live::admit_cast_type_name(decoder.blob()?, 1, &mut |bytes, units| {
						budget.admit(bytes)?;
						work.take(
							depth,
							usize::try_from(units)
								.map_err(|_error| invalid("LIVE call type work overflows"))?,
						)
					})
					.map_err(|error| {
						invalid("LIVE call type is malformed or exceeds limits").with_cause(error)
					})?;
				Some(target.into())
			} else {
				None
			};
			let count = values::count(decoder, 2)?;
			work.take(depth, count)?;
			budget.admit(
				count
					.checked_mul(std::mem::size_of::<(Option<crate::values::Text>, LiveExpr)>())
					.ok_or_else(|| invalid("LIVE call size overflows"))?,
			)?;
			let mut arguments = Vec::new();
			arguments
				.try_reserve_exact(count)
				.map_err(|_error| invalid("LIVE arguments allocation failed"))?;
			for _ in 0..count {
				let name = match decoder.u8()? {
					0 => None,
					1 => {
						let name = values::text(decoder, budget)?;
						if name.is_empty() {
							return Err(invalid("LIVE argument name is empty"));
						}
						Some(name)
					}
					_ => return Err(invalid("LIVE argument name flag is invalid")),
				};
				arguments.push((name, decode_expr(decoder, context, budget, work, depth + 1)?));
			}
			LiveExpr::CallWithOptions {
				name,
				type_argument,
				distinct: flags & 2 != 0,
				arguments,
			}
		}
		18 => {
			let flags = decoder.u8()?;
			if flags & !7 != 0 {
				return Err(invalid("LIVE range flags contain reserved bits"));
			}
			LiveExpr::Range {
				start: (flags & 1 != 0)
					.then(|| decode_expr(decoder, context, budget, work, depth + 1).map(Box::new))
					.transpose()?,
				end: (flags & 2 != 0)
					.then(|| decode_expr(decoder, context, budget, work, depth + 1).map(Box::new))
					.transpose()?,
				inclusive_end: flags & 4 != 0,
			}
		}
		17 => {
			let target =
				crate::live::admit_cast_type_name(decoder.blob()?, 1, &mut |bytes, units| {
					budget.admit(bytes)?;
					work.take(
						depth,
						usize::try_from(units)
							.map_err(|_error| invalid("LIVE cast work overflows"))?,
					)?;
					Ok(())
				})
				.map_err(|error| {
					invalid("LIVE cast type is malformed or exceeds limits").with_cause(error)
				})?;
			LiveExpr::Cast {
				target: target.into(),
				value: Box::new(decode_expr(decoder, context, budget, work, depth + 1)?),
			}
		}
		0 => LiveExpr::Literal(values::decode(decoder.blob()?, context, budget)?),
		1 => LiveExpr::Field(decode_path(decoder, budget, work)?),
		2 => {
			let name = values::text(decoder, budget)?;
			if name.is_empty() {
				return Err(invalid("LIVE parameter name is empty"));
			}
			LiveExpr::Parameter(name)
		}
		3 => LiveExpr::Binding(LiveBindingId::from_uuid(
			uuid::Uuid::from_slice(decoder.take(16)?)
				.map_err(|_error| invalid("LIVE binding UUID is malformed"))?
				.into(),
		)?),
		4 => LiveExpr::Not(Box::new(decode_expr(decoder, context, budget, work, depth + 1)?)),
		5 => {
			let op = decode_operator(decoder.u8()?)?;
			let left = Box::new(decode_expr(decoder, context, budget, work, depth + 1)?);
			let right = Box::new(decode_expr(decoder, context, budget, work, depth + 1)?);
			LiveExpr::Binary {
				left,
				op,
				right,
			}
		}
		6 => {
			let name = values::text(decoder, budget)?;
			if name.is_empty() {
				return Err(invalid("LIVE function name is empty"));
			}
			let count = values::count(decoder, 1)?;
			work.take(depth, count)?;
			budget.admit(count * std::mem::size_of::<LiveExpr>())?;
			let mut arguments = Vec::new();
			arguments
				.try_reserve_exact(count)
				.map_err(|_error| invalid("LIVE arguments allocation failed"))?;
			for _ in 0..count {
				arguments.push(decode_expr(decoder, context, budget, work, depth + 1)?);
			}
			LiveExpr::Call {
				name,
				arguments,
			}
		}
		7 => LiveExpr::Select(Box::new(decode_query(decoder, context, budget, work, depth + 1)?)),
		tag @ (8 | 15) => {
			let count = values::count(decoder, 1)?;
			work.take(depth, count)?;
			budget.admit(
				count
					.checked_mul(std::mem::size_of::<LiveExpr>())
					.ok_or_else(|| invalid("LIVE array size overflows"))?,
			)?;
			let mut values = Vec::new();
			values
				.try_reserve_exact(count)
				.map_err(|_error| invalid("LIVE array allocation failed"))?;
			for _ in 0..count {
				values.push(decode_expr(decoder, context, budget, work, depth + 1)?);
			}
			if tag == 15 {
				LiveExpr::Set(values)
			} else {
				LiveExpr::Array(values)
			}
		}
		9 => {
			let count = values::count(decoder, 9)?;
			work.take(depth, count)?;
			budget.admit(
				count
					.checked_mul(std::mem::size_of::<(crate::values::Text, LiveExpr)>())
					.ok_or_else(|| invalid("LIVE object size overflows"))?,
			)?;
			let mut fields = Vec::new();
			fields
				.try_reserve_exact(count)
				.map_err(|_error| invalid("LIVE object allocation failed"))?;
			for _ in 0..count {
				let name = values::text(decoder, budget)?;
				fields.push((name, decode_expr(decoder, context, budget, work, depth + 1)?));
			}
			LiveExpr::Object(fields)
		}
		10 => LiveExpr::Negate(Box::new(decode_expr(decoder, context, budget, work, depth + 1)?)),
		11 => LiveExpr::Positive(Box::new(decode_expr(decoder, context, budget, work, depth + 1)?)),
		12 => {
			let null_safe = match decoder.u8()? {
				0 => false,
				1 => true,
				_ => return Err(invalid("LIVE null-safe flag is invalid")),
			};
			let field = values::text(decoder, budget)?;
			let base = Box::new(decode_expr(decoder, context, budget, work, depth + 1)?);
			LiveExpr::Access {
				base,
				field,
				null_safe,
			}
		}
		13 => {
			let base = Box::new(decode_expr(decoder, context, budget, work, depth + 1)?);
			let index = Box::new(decode_expr(decoder, context, budget, work, depth + 1)?);
			LiveExpr::Index {
				base,
				index,
			}
		}
		14 => LiveExpr::Conditional {
			condition: Box::new(decode_expr(decoder, context, budget, work, depth + 1)?),
			then_value: Box::new(decode_expr(decoder, context, budget, work, depth + 1)?),
			else_value: Box::new(decode_expr(decoder, context, budget, work, depth + 1)?),
		},
		16 => {
			let operand = match decoder.u8()? {
				0 => None,
				1 => Some(Box::new(decode_expr(decoder, context, budget, work, depth + 1)?)),
				_ => return Err(invalid("LIVE CASE operand flag is invalid")),
			};
			let count = values::count(decoder, 2)?;
			if count == 0 {
				return Err(invalid("LIVE CASE requires a branch"));
			}
			work.take(
				depth,
				count.checked_mul(2).ok_or_else(|| invalid("LIVE CASE count overflows"))?,
			)?;
			budget.admit(
				count
					.checked_mul(std::mem::size_of::<(LiveExpr, LiveExpr)>())
					.ok_or_else(|| invalid("LIVE CASE size overflows"))?,
			)?;
			let mut branches = Vec::new();
			branches
				.try_reserve_exact(count)
				.map_err(|_error| invalid("LIVE CASE allocation failed"))?;
			for _ in 0..count {
				branches.push((
					decode_expr(decoder, context, budget, work, depth + 1)?,
					decode_expr(decoder, context, budget, work, depth + 1)?,
				));
			}
			let else_value = match decoder.u8()? {
				0 => None,
				1 => Some(Box::new(decode_expr(decoder, context, budget, work, depth + 1)?)),
				_ => return Err(invalid("LIVE CASE ELSE flag is invalid")),
			};
			LiveExpr::Case {
				operand,
				branches,
				else_value,
			}
		}
		_ => return Err(invalid("LIVE expression tag is unknown")),
	})
}

/// Stable operation tags; never derive bytes from Rust enum discriminants.
const fn operator_tag(operator: LiveBinaryOperator) -> u8 {
	match operator {
		LiveBinaryOperator::Equal => 0,
		LiveBinaryOperator::NotEqual => 1,
		LiveBinaryOperator::Less => 2,
		LiveBinaryOperator::LessOrEqual => 3,
		LiveBinaryOperator::Greater => 4,
		LiveBinaryOperator::GreaterOrEqual => 5,
		LiveBinaryOperator::And => 6,
		LiveBinaryOperator::Or => 7,
		LiveBinaryOperator::Add => 8,
		LiveBinaryOperator::Subtract => 9,
		LiveBinaryOperator::Multiply => 10,
		LiveBinaryOperator::Divide => 11,
		LiveBinaryOperator::In => 12,
		LiveBinaryOperator::Coalesce => 13,
		LiveBinaryOperator::Power => 14,
		LiveBinaryOperator::Remainder => 15,
		LiveBinaryOperator::Is => 16,
		LiveBinaryOperator::NotIs => 17,
		LiveBinaryOperator::NotIn => 18,
		LiveBinaryOperator::AllIn => 19,
		LiveBinaryOperator::AnyIn => 20,
		LiveBinaryOperator::NoneIn => 21,
		LiveBinaryOperator::AnyEqual => 22,
		LiveBinaryOperator::AllEqual => 23,
		LiveBinaryOperator::Contains => 24,
		LiveBinaryOperator::ContainsNot => 25,
		LiveBinaryOperator::ContainsAny => 26,
		LiveBinaryOperator::ContainsAll => 27,
		LiveBinaryOperator::Elvis => 28,
		LiveBinaryOperator::Inside => 29,
		LiveBinaryOperator::NotInside => 30,
		LiveBinaryOperator::AllInside => 31,
		LiveBinaryOperator::AnyInside => 32,
		LiveBinaryOperator::NoneInside => 33,
		LiveBinaryOperator::Outside => 34,
		LiveBinaryOperator::Intersects => 35,
	}
}

/// Rejects unknown operation tags rather than silently selecting a fallback.
fn decode_operator(tag: u8) -> FerrosGrynnResult<LiveBinaryOperator> {
	Ok(match tag {
		0 => LiveBinaryOperator::Equal,
		1 => LiveBinaryOperator::NotEqual,
		2 => LiveBinaryOperator::Less,
		3 => LiveBinaryOperator::LessOrEqual,
		4 => LiveBinaryOperator::Greater,
		5 => LiveBinaryOperator::GreaterOrEqual,
		6 => LiveBinaryOperator::And,
		7 => LiveBinaryOperator::Or,
		8 => LiveBinaryOperator::Add,
		9 => LiveBinaryOperator::Subtract,
		10 => LiveBinaryOperator::Multiply,
		11 => LiveBinaryOperator::Divide,
		12 => LiveBinaryOperator::In,
		13 => LiveBinaryOperator::Coalesce,
		14 => LiveBinaryOperator::Power,
		15 => LiveBinaryOperator::Remainder,
		16 => LiveBinaryOperator::Is,
		17 => LiveBinaryOperator::NotIs,
		18 => LiveBinaryOperator::NotIn,
		19 => LiveBinaryOperator::AllIn,
		20 => LiveBinaryOperator::AnyIn,
		21 => LiveBinaryOperator::NoneIn,
		22 => LiveBinaryOperator::AnyEqual,
		23 => LiveBinaryOperator::AllEqual,
		24 => LiveBinaryOperator::Contains,
		25 => LiveBinaryOperator::ContainsNot,
		26 => LiveBinaryOperator::ContainsAny,
		27 => LiveBinaryOperator::ContainsAll,
		28 => LiveBinaryOperator::Elvis,
		29 => LiveBinaryOperator::Inside,
		30 => LiveBinaryOperator::NotInside,
		31 => LiveBinaryOperator::AllInside,
		32 => LiveBinaryOperator::AnyInside,
		33 => LiveBinaryOperator::NoneInside,
		34 => LiveBinaryOperator::Outside,
		35 => LiveBinaryOperator::Intersects,
		_ => return Err(invalid("LIVE binary operation tag is unknown")),
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::values::Uuid;

	/// Caller cancellation/work rejection occurs during admission, before
	/// copying payloads.
	#[test]
	fn borrowed_descriptor_controlled_payload_and_cancellation() -> FerrosGrynnResult<()> {
		let expression = LiveExpr::Literal(Value::from("x".repeat(32 * 1024)));
		let parameters = Object::new();
		let descriptor = LiveDescriptorRef::Binding {
			expression: &expression,
			parameters: &parameters,
		};
		let owned = LiveDescriptor::Binding {
			expression: expression.clone(),
			parameters: parameters.clone(),
		};
		let work = std::cell::Cell::new(0_u64);
		let observe = |amount| {
			let next = work.get() + amount;
			work.set(next);
			if next > 128 {
				Err(invalid("test caller work exceeded"))
			} else {
				Ok(())
			}
		};
		assert!(encode_ref_controlled(descriptor, 1 << 20, &observe).is_err());
		assert!(work.get() > 128);
		assert!(
			encode_ref_controlled(descriptor, 1 << 20, &|_| Err(invalid("test caller cancelled")))
				.is_err()
		);
		assert_eq!(
			encode_ref_controlled(descriptor, 1 << 20, &|_| Ok(()))?,
			encode(&owned, 1 << 20)?
		);
		Ok(())
	}

	/// Scope recipes share canonical depth/byte guards with WHERE expressions.
	#[test]
	fn live_scope_expression_descriptors_are_checked() -> FerrosGrynnResult<()> {
		let binding = LiveBindingId::from_uuid(Uuid::from(uuid::Uuid::from_u128(42)))?;
		let class = crate::datastore::ClassId::from_storage(1)?;
		for source in [
			LiveSource::GraphExpr(Box::new(LiveExpr::Binding(binding))),
			LiveSource::GraphClassExpr {
				graph: Box::new(LiveExpr::Binding(binding)),
				class,
			},
			LiveSource::GraphRecordExpr {
				graph: Box::new(LiveExpr::Binding(binding)),
				record: "person:a".parse()?,
			},
		] {
			let query = LiveQuery::new(source);
			let mut body = Vec::new();
			let mut context = EncodeContext::default();
			encode_query(
				&mut body,
				&query,
				&mut context,
				&mut CodecBudget::new(1 << 20, 0)?,
				&mut Work {
					observe: None,
					remaining: MAX_NODES,
				},
				0,
			)?;
			let (binding_start, expected) = match query.source() {
				LiveSource::GraphExpr(_) => (2, vec![7, 3]),
				LiveSource::GraphClassExpr {
					..
				} => (6, vec![5, 1, 0, 0, 0, 3]),
				LiveSource::GraphRecordExpr {
					..
				} => (2, vec![6, 3]),
				_ => return Err(invalid("scope codec fixture has unexpected source")),
			};
			assert_eq!(&body[..binding_start], expected.as_slice());
			assert_eq!(
				&body[binding_start..binding_start + 16],
				binding.as_uuid().as_inner().as_bytes()
			);
			let mut nil = body.clone();
			nil[binding_start..binding_start + 16].fill(0);
			assert!(
				decode_query(
					&mut Decoder::new(&nil),
					&context,
					&mut CodecBudget::new(1 << 20, 0)?,
					&mut Work {
						observe: None,
						remaining: MAX_NODES
					},
					0
				)
				.is_err()
			);
			let descriptor = LiveDescriptor::Query(query);
			let bytes = encode(&descriptor, 1 << 20)?;
			assert_eq!(decode(&bytes, 1 << 20)?, descriptor);
			for end in 0..bytes.len() {
				assert!(decode(&bytes[..end], 1 << 20).is_err());
			}
			assert!(encode(&descriptor, 64).is_err());
			assert!(decode(&bytes, 64).is_err());
		}
		let mut graph = LiveExpr::Literal(Value::Null);
		for _ in 0..65 {
			graph = LiveExpr::Not(Box::new(graph));
		}
		for source in [
			LiveSource::GraphClassExpr {
				graph: Box::new(graph.clone()),
				class,
			},
			LiveSource::GraphExpr(Box::new(graph)),
		] {
			assert!(encode(&LiveDescriptor::Query(LiveQuery::new(source)), 1 << 20).is_err());
		}
		Ok(())
	}

	#[test]
	fn borrowed_binding_value_matches_canonical_logical_descriptor() -> FerrosGrynnResult<()> {
		for value in [
			Value::from(9_007_199_254_740_993_i64),
			Value::Null,
			Value::from("exact historical value"),
		] {
			let expected = encode(
				&LiveDescriptor::Binding {
					expression: LiveExpr::Literal(value.clone()),
					parameters: Object::new(),
				},
				1024 * 1024,
			)?;
			let actual = encode_binding_value(&value, 1024 * 1024)?;
			assert_eq!(actual, expected);
			assert_eq!(decode(&actual, 1024 * 1024)?, LiveDescriptor::Binding {
				expression: LiveExpr::Literal(value),
				parameters: Object::new()
			});
		}
		assert!(encode_binding_value(&Value::from("oversized"), 16).is_err());
		Ok(())
	}

	#[test]
	fn golden_logical_descriptor_rejects_malformed_frames() -> FerrosGrynnResult<()> {
		let query = LiveQuery::new(LiveSource::Class(
			ClassId::new(1).ok_or_else(|| invalid("test class invalid"))?,
		));
		let descriptor = LiveDescriptor::Query(query);
		let bytes = encode(&descriptor, 1024 * 1024)?;
		let mut golden = b"FGLD\x01\x00\x00\x00".to_vec();
		golden.extend_from_slice(&16_u64.to_le_bytes());
		golden.extend_from_slice(&27_u64.to_le_bytes());
		golden.extend_from_slice(&[0; 16]);
		golden.extend_from_slice(&[0, 1, 0, 0, 0]);
		golden.extend_from_slice(&[0; 22]);
		assert_eq!(bytes, golden);
		assert_eq!(decode(&bytes, 1024 * 1024)?, descriptor);
		for length in 0..bytes.len() {
			assert!(decode(&bytes[..length], 1024 * 1024).is_err());
		}
		for offset in [0, 4, 6, 7, 8, 16, 40, 45, 46, 47, 51, 52, 53, 54, 55, 59, 63, 66] {
			let mut bad = bytes.clone();
			bad[offset] = 255;
			assert!(decode(&bad, 1024 * 1024).is_err(), "offset {offset}");
		}
		let mut trailing = bytes.clone();
		trailing.push(0);
		assert!(decode(&trailing, 1024 * 1024).is_err());
		assert!(encode(&descriptor, 8).is_err());
		assert!(decode(&bytes, bytes.len()).is_err());
		Ok(())
	}

	/// Source aliases add a checked optional tail without changing default
	/// bytes.
	#[test]
	fn live_window_source_alias_descriptor_tail() -> FerrosGrynnResult<()> {
		let base = LiveQuery::new(LiveSource::Class(ClassId::from_storage(1)?));
		let descriptor = LiveDescriptor::Query(base.clone().with_alias("p")?);
		let frame = encode(&descriptor, 1 << 20)?;
		assert_eq!(frame.len(), 76);
		assert_eq!(frame[66], 16);
		assert_eq!(&frame[67..75], &1_u64.to_le_bytes());
		assert_eq!(frame[75], b'p');
		assert_eq!(decode(&frame, 1 << 20)?, descriptor);
		assert_eq!(encode(&LiveDescriptor::Query(base.clone()), 1 << 20)?.len(), 67);
		for byte in [0, 255] {
			let mut bad = frame.clone();
			bad[75] = byte;
			assert!(decode(&bad, 1 << 20).is_err());
		}
		for length in [0_u64, u64::MAX] {
			let mut bad = frame.clone();
			bad[67..75].copy_from_slice(&length.to_le_bytes());
			assert!(decode(&bad, 1 << 20).is_err());
		}
		for flag in [32, 64, 128, 255] {
			let mut bad = frame.clone();
			bad[66] = flag;
			assert!(decode(&bad, 1 << 20).is_err());
		}
		for length in 0..frame.len() {
			assert!(decode(&frame[..length], 1 << 20).is_err());
		}
		assert!(encode(&descriptor, 64).is_err());
		assert!(decode(&frame, frame.len()).is_err());
		let nested = LiveDescriptor::Query(
			base.with_alias("p")?
				.with_split(vec![LiveExpr::Field(LivePath::new(vec!["p".into(), "tags".into()])?)])
				.with_projection(LiveProjection::Value(LiveExpr::Select(Box::new(
					LiveQuery::new(LiveSource::Record("person:a".parse()?)).with_alias("p")?,
				)))),
		);
		assert_eq!(decode(&encode(&nested, 1 << 20)?, 1 << 20)?, nested);
		Ok(())
	}

	/// Query flags keep default bytes and reject reserved bits before
	/// publication.
	#[test]
	fn live_window_only_descriptor_flags() -> FerrosGrynnResult<()> {
		let base = LiveQuery::new(LiveSource::Class(ClassId::from_storage(1)?));
		for (mode, output) in
			[(LiveOutputMode::Full, 0_u8), (LiveOutputMode::Diff, 1), (LiveOutputMode::Patch, 2)]
		{
			for only in [false, true] {
				let query = base.clone().with_output(mode).with_only(only);
				let mut body = Vec::new();
				let mut context = EncodeContext::default();
				encode_query(
					&mut body,
					&query,
					&mut context,
					&mut CodecBudget::new(1 << 20, 0)?,
					&mut Work {
						observe: None,
						remaining: MAX_NODES,
					},
					0,
				)?;
				assert_eq!(body.len(), 27);
				assert_eq!(body.last(), Some(&(output | (u8::from(only) << 2))));
				let decoded = decode_query(
					&mut Decoder::new(&body),
					&context,
					&mut CodecBudget::new(1 << 20, 0)?,
					&mut Work {
						observe: None,
						remaining: MAX_NODES,
					},
					0,
				)?;
				assert_eq!(decoded, query);
				for flags in [3, 7, 8, 16, 128, 255] {
					let mut bad = body.clone();
					*bad.last_mut().ok_or_else(|| invalid("ONLY golden flag absent"))? = flags;
					assert!(
						decode_query(
							&mut Decoder::new(&bad),
							&context,
							&mut CodecBudget::new(1 << 20, 0)?,
							&mut Work {
								observe: None,
								remaining: MAX_NODES
							},
							0
						)
						.is_err()
					);
				}
				let descriptor = LiveDescriptor::Query(query.clone());
				let frame = encode(&descriptor, 1 << 20)?;
				assert_eq!(frame.len(), 67);
				assert_eq!(decode(&frame, 1 << 20)?, descriptor);
				for length in 0..frame.len() {
					assert!(decode(&frame[..length], 1 << 20).is_err());
				}
				let nested = LiveDescriptor::Query(
					LiveQuery::new(LiveSource::Class(ClassId::from_storage(1)?))
						.with_projection(LiveProjection::Value(LiveExpr::Select(Box::new(query)))),
				);
				assert_eq!(decode(&encode(&nested, 1 << 20)?, 1 << 20)?, nested);
			}
		}
		Ok(())
	}

	/// An optional SPLIT tail preserves default bytes and validates its own
	/// positive count, flags, nesting and retained allocation before decoding.
	#[test]
	fn live_window_split_descriptor_tail() -> FerrosGrynnResult<()> {
		let base = LiveQuery::new(LiveSource::Class(ClassId::from_storage(1)?));
		let mut context = EncodeContext::default();
		let query = base.clone().with_split(vec![LiveExpr::Range {
			start: None,
			end: None,
			inclusive_end: false,
		}]);
		let mut body = Vec::new();
		encode_query(
			&mut body,
			&query,
			&mut context,
			&mut CodecBudget::new(1 << 20, 0)?,
			&mut Work {
				observe: None,
				remaining: MAX_NODES,
			},
			0,
		)?;
		assert_eq!(&body[26..31], &[8, 1, 0, 0, 0]);
		assert_eq!(body.len(), 33);
		let decode_body = |bytes: &[u8], maximum, work| -> FerrosGrynnResult<_> {
			decode_query(
				&mut Decoder::new(bytes),
				&context,
				&mut CodecBudget::new(maximum, 0)?,
				&mut Work {
					observe: None,
					remaining: work,
				},
				0,
			)
		};
		assert_eq!(decode_body(&body, 1 << 20, MAX_NODES)?, query);
		for count in [0_u32, u32::MAX] {
			let mut bad = body.clone();
			bad[27..31].copy_from_slice(&count.to_le_bytes());
			assert!(decode_body(&bad, 1 << 20, MAX_NODES).is_err());
		}
		for flags in [3, 11, 16, 255] {
			let mut bad = body.clone();
			bad[26] = flags;
			assert!(decode_body(&bad, 1 << 20, MAX_NODES).is_err());
		}
		assert!(decode_body(&body, 1, MAX_NODES).is_err());
		assert!(decode_body(&body, 1 << 20, 1).is_err());
		for size in 0..body.len() {
			assert!(decode_body(&body[..size], 1 << 20, MAX_NODES).is_err());
		}
		for split in [vec![], vec![LiveExpr::field("tags")?], vec![
			LiveExpr::field("left")?,
			LiveExpr::field("right")?,
		]] {
			let query = base.clone().with_split(split);
			let descriptor = LiveDescriptor::Query(query.clone());
			assert_eq!(decode(&encode(&descriptor, 1 << 20)?, 1 << 20)?, descriptor);
			let nested = LiveDescriptor::Query(
				base.clone()
					.with_projection(LiveProjection::Value(LiveExpr::Select(Box::new(query)))),
			);
			assert_eq!(decode(&encode(&nested, 1 << 20)?, 1 << 20)?, nested);
		}
		let empty = LiveDescriptor::Query(base);
		assert_eq!(encode(&empty, 1 << 20)?.len(), 67);
		Ok(())
	}

	#[test]
	fn descriptors_preserve_exact_values_bindings_and_nested_queries() -> FerrosGrynnResult<()> {
		let binding = LiveBindingId::from_uuid(Uuid::now_v7())?;
		let mut parameters = Object::new();
		parameters.insert("age", (1_u64 << 53) + 1);
		let query = LiveQuery::new(LiveSource::Record("person:alice".parse()?))
			.with_filter(
				LiveExpr::field("age")?
					.binary(LiveBinaryOperator::GreaterOrEqual, LiveExpr::Binding(binding)),
			)
			.with_fetch(vec![LivePath::new(vec!["manager".into(), "address".into()])?])
			.with_omit(vec![
				LivePath::field("id")?,
				LivePath::new(vec!["private".into(), "token".into()])?,
			])
			.with_projection(LiveProjection::Fields(vec![LiveField {
				expression: LiveExpr::Parameter("age".into()),
				alias: Some(LivePath::field("exact")?),
			}]))
			.with_parameters(parameters.clone())
			.with_group_by(vec![LiveExpr::Binding(binding), LiveExpr::Parameter("age".into())])?
			.with_output(LiveOutputMode::Patch);
		for descriptor in [LiveDescriptor::Query(query.clone()), LiveDescriptor::Binding {
			expression: LiveExpr::Select(Box::new(query)),
			parameters,
		}] {
			let bytes = encode(&descriptor, 1024 * 1024)?;
			assert_eq!(encode_ref(descriptor.as_ref(), 1024 * 1024)?, bytes);
			assert!(encode_ref(descriptor.as_ref(), 16).is_err());
			assert_eq!(decode(&bytes, 1024 * 1024)?, descriptor);
			assert_eq!(encode(&decode(&bytes, 1024 * 1024)?, 1024 * 1024)?, bytes);
		}
		Ok(())
	}

	/// Output paths retain order, boundaries and repetitions in canonical
	/// bytes.
	#[test]
	fn live_omit_descriptor_checked_paths() -> FerrosGrynnResult<()> {
		let base = LiveQuery::new(LiveSource::Class(ClassId::from_storage(1)?));
		let nested = LivePath::new(vec!["private".into(), "token".into()])?;
		let query = base
			.clone()
			.with_omit(vec![nested.clone(), LivePath::field("literal.dot")?, nested])
			.with_fetch(vec![LivePath::field("manager")?]);
		let descriptor = LiveDescriptor::Query(query.clone());
		let bytes = encode(&descriptor, 1 << 20)?;
		assert_eq!(decode(&bytes, 1 << 20)?, descriptor);
		assert_ne!(bytes, encode(&LiveDescriptor::Query(base.clone()), 1 << 20)?);
		assert_ne!(
			bytes,
			encode(
				&LiveDescriptor::Query(query.with_omit(vec![LivePath::field("other")?])),
				1 << 20
			)?
		);
		for end in 0..bytes.len() {
			assert!(decode(&bytes[..end], 1 << 20).is_err());
		}
		assert!(decode(&bytes, 64).is_err());
		let query = base.with_omit(vec![LivePath::field("x")?]);
		let mut body = Vec::new();
		let mut context = EncodeContext::default();
		encode_query(
			&mut body,
			&query,
			&mut context,
			&mut CodecBudget::new(1 << 20, 0)?,
			&mut Work {
				observe: None,
				remaining: MAX_NODES,
			},
			0,
		)?;
		assert_eq!(&body[14..18], &1_u32.to_le_bytes());
		assert_eq!(&body[18..22], &1_u32.to_le_bytes());
		assert_eq!(&body[22..30], &1_u64.to_le_bytes());
		assert_eq!(body[30], b'x');
		let decode_body = |bytes: &[u8]| {
			decode_query(
				&mut Decoder::new(bytes),
				&context,
				&mut CodecBudget::new(1 << 20, 0)?,
				&mut Work {
					observe: None,
					remaining: MAX_NODES,
				},
				0,
			)
		};
		for (start, length, number) in [
			(14, 4, u64::from(u32::MAX)),
			(18, 4, 0),
			(18, 4, 65),
			(18, 4, u64::from(u32::MAX)),
			(22, 8, u64::MAX),
			(22, 8, 0),
		] {
			let mut bad = body.clone();
			bad[start..start + length].copy_from_slice(&number.to_le_bytes()[..length]);
			assert!(decode_body(&bad).is_err(), "offset {start}, value {number}");
		}
		for byte in [0, 255] {
			let mut bad = body.clone();
			bad[30] = byte;
			assert!(decode_body(&bad).is_err());
		}
		assert!(
			encode_query(
				&mut Vec::new(),
				&query,
				&mut context,
				&mut CodecBudget::new(1 << 20, 0)?,
				&mut Work {
					observe: None,
					remaining: 0
				},
				0
			)
			.is_err()
		);
		Ok(())
	}

	/// Clause tags, counts and recursive keys are checked before allocation.
	#[test]
	fn live_group_descriptor_checked_layout() -> FerrosGrynnResult<()> {
		let base = LiveQuery::new(LiveSource::Class(ClassId::from_storage(1)?));
		for query in [
			base.clone(),
			base.clone().with_group_all(),
			base.clone().with_group_by(vec![
				LiveExpr::field("k")?,
				LiveExpr::Literal(9_007_199_254_740_993_i64.into()),
			])?,
		] {
			let descriptor = LiveDescriptor::Query(query.clone());
			let bytes = encode(&descriptor, 1 << 20)?;
			assert_eq!(decode(&bytes, 1 << 20)?, descriptor);
			for end in 0..bytes.len() {
				assert!(decode(&bytes[..end], 1 << 20).is_err());
			}
			assert!(decode(&bytes, 64).is_err());
			let mut body = Vec::new();
			let mut context = EncodeContext::default();
			encode_query(
				&mut body,
				&query,
				&mut context,
				&mut CodecBudget::new(1 << 20, 0)?,
				&mut Work {
					observe: None,
					remaining: MAX_NODES,
				},
				0,
			)?;
			assert_eq!(body[6], match query.group() {
				None => 0,
				Some([]) => 1,
				Some(_) => 2,
			});
			let mut bad = body.clone();
			bad[6] = 3;
			assert!(
				decode_query(
					&mut Decoder::new(&bad),
					&context,
					&mut CodecBudget::new(1 << 20, 0)?,
					&mut Work {
						observe: None,
						remaining: MAX_NODES
					},
					0
				)
				.is_err()
			);
			if body[6] == 2 {
				assert_eq!(&body[7..11], &2_u32.to_le_bytes());
				for count in [0, u32::MAX] {
					let mut bad = body.clone();
					bad[7..11].copy_from_slice(&count.to_le_bytes());
					assert!(
						decode_query(
							&mut Decoder::new(&bad),
							&context,
							&mut CodecBudget::new(1 << 20, 0)?,
							&mut Work {
								observe: None,
								remaining: MAX_NODES
							},
							0
						)
						.is_err()
					);
				}
			}
		}
		let mut key = LiveExpr::Literal(Value::None);
		for _ in 0..65 {
			key = LiveExpr::Not(Box::new(key));
		}
		assert!(encode(&LiveDescriptor::Query(base.with_group_by(vec![key])?), 1 << 20).is_err());
		Ok(())
	}

	/// Source tags are independent of Rust discriminants; graph slots and exact
	/// RecordId values are checked before any descriptor can reach the
	/// registry.
	#[test]
	fn live_graph_record_codec() -> FerrosGrynnResult<()> {
		use crate::graph::{GraphId, GraphKind, GraphRef, GraphStorageMode};
		let graph = GraphRef::new(
			GraphId::new(7).ok_or_else(|| invalid("test graph invalid"))?,
			"scope",
			GraphKind::Graph,
			GraphStorageMode::InMemory,
		)?;
		let record = "person:alice".parse()?;
		let query = LiveQuery::new(LiveSource::GraphRecord {
			graph: graph.clone(),
			record,
		});
		let descriptor = LiveDescriptor::Query(query.clone());
		let frame = encode(&descriptor, 1 << 20)?;
		assert_eq!(decode(&frame, 1 << 20)?, descriptor);
		for length in 0..frame.len() {
			assert!(decode(&frame[..length], 1 << 20).is_err());
		}
		let mut body = Vec::new();
		let mut context = EncodeContext::default();
		let mut budget = CodecBudget::new(1 << 20, 0)?;
		encode_query(
			&mut body,
			&query,
			&mut context,
			&mut budget,
			&mut Work {
				observe: None,
				remaining: MAX_NODES,
			},
			0,
		)?;
		let mut golden = vec![4];
		golden.extend_from_slice(&1_u64.to_le_bytes());
		let mut value_context = EncodeContext::default();
		let mut value_budget = CodecBudget::new(1 << 20, 0)?;
		encode_value(
			&mut golden,
			&Value::from("person:alice".parse::<crate::values::RecordId>()?),
			&mut value_context,
			&mut value_budget,
			None,
		)?;
		golden.extend_from_slice(&[0; 22]);
		assert_eq!(body, golden);
		for slot in [0_u64, 2, u64::MAX] {
			let mut bad = body.clone();
			bad[1..9].copy_from_slice(&slot.to_le_bytes());
			assert!(
				decode_query(
					&mut Decoder::new(&bad),
					&context,
					&mut CodecBudget::new(1 << 20, 0)?,
					&mut Work {
						observe: None,
						remaining: MAX_NODES
					},
					0
				)
				.is_err()
			);
		}
		let mut wrong_type = body[..9].to_vec();
		encode_value(&mut wrong_type, &Value::Bool(true), &mut context, &mut budget, None)?;
		wrong_type.extend_from_slice(&[0; 22]);
		assert!(
			decode_query(
				&mut Decoder::new(&wrong_type),
				&context,
				&mut CodecBudget::new(1 << 20, 0)?,
				&mut Work {
					observe: None,
					remaining: MAX_NODES
				},
				0
			)
			.is_err()
		);
		assert!(encode(&descriptor, 16).is_err());
		assert!(decode(&frame, frame.len()).is_err());
		Ok(())
	}

	/// New logical nodes preserve ordered fields and reject untrusted counts.
	#[test]
	fn live_recipe_collections_and_unary_codec() -> FerrosGrynnResult<()> {
		for (tag, expression) in [
			(10, LiveExpr::Negate(Box::new(LiveExpr::Literal(1.into())))),
			(11, LiveExpr::Positive(Box::new(LiveExpr::Literal(1.into())))),
		] {
			let descriptor = LiveDescriptor::Binding {
				expression,
				parameters: Object::new(),
			};
			let bytes = encode(&descriptor, 1024 * 1024)?;
			let mut golden = encode_binding_value(&Value::from(1), 1024 * 1024)?;
			let length = u64::from_le_bytes(
				golden[16..24]
					.try_into()
					.map_err(|_error| invalid("golden body length missing"))?,
			);
			golden[16..24].copy_from_slice(&(length + 1).to_le_bytes());
			golden.insert(40, tag);
			assert_eq!(bytes, golden);
			assert_eq!(decode(&bytes, 1024 * 1024)?, descriptor);
		}
		let ordered = LiveDescriptor::Binding {
			expression: LiveExpr::Object(
				["z", "a", "z"]
					.into_iter()
					.map(|key| (key.into(), LiveExpr::Array(Vec::new())))
					.collect(),
			),
			parameters: Object::new(),
		};
		let mut golden = b"FGLD\x01\x00\x01\x00".to_vec();
		golden.extend_from_slice(&16_u64.to_le_bytes());
		golden.extend_from_slice(&51_u64.to_le_bytes());
		golden.extend_from_slice(&[0; 16]);
		golden.extend_from_slice(&[9, 3, 0, 0, 0]);
		for key in [b'z', b'a', b'z'] {
			golden.extend_from_slice(&1_u64.to_le_bytes());
			golden.extend_from_slice(&[key, 8, 0, 0, 0, 0]);
		}
		golden.extend_from_slice(&[0; 4]);
		assert_eq!(encode(&ordered, 1024 * 1024)?, golden);
		assert_eq!(decode(&golden, 1024 * 1024)?, ordered);
		for (tag, expression) in
			[(8, LiveExpr::Array(Vec::new())), (9, LiveExpr::Object(Vec::new()))]
		{
			let descriptor = LiveDescriptor::Binding {
				expression,
				parameters: Object::new(),
			};
			let bytes = encode(&descriptor, 1024 * 1024)?;
			let mut golden = b"FGLD\x01\x00\x01\x00".to_vec();
			golden.extend_from_slice(&16_u64.to_le_bytes());
			golden.extend_from_slice(&9_u64.to_le_bytes());
			golden.extend_from_slice(&[0; 16]);
			golden.push(tag);
			golden.extend_from_slice(&[0; 8]);
			assert_eq!(bytes, golden);
			assert_eq!(decode(&bytes, 1024 * 1024)?, descriptor);
			let mut bad = bytes.clone();
			bad[41..45].copy_from_slice(&u32::MAX.to_le_bytes());
			assert!(decode(&bad, 1024 * 1024).is_err());
			assert!(decode(&bytes, bytes.len()).is_err());
		}
		let id = LiveBindingId::from_uuid(Uuid::now_v7())?;
		let descriptor = LiveDescriptor::Binding {
			expression: LiveExpr::Object(vec![
				("same".into(), LiveExpr::Literal(0.into())),
				(
					"same".into(),
					LiveExpr::Array(vec![
						LiveExpr::Negate(Box::new(LiveExpr::Binding(id))),
						LiveExpr::Positive(Box::new(LiveExpr::Parameter("fixed".into()))),
					]),
				),
			]),
			parameters: Object::from_iter([("fixed", Value::from(9_007_199_254_740_993_i64))]),
		};
		let bytes = encode(&descriptor, 1024 * 1024)?;
		assert_eq!(decode(&bytes, 1024 * 1024)?, descriptor);
		for length in 0..bytes.len() {
			assert!(decode(&bytes[..length], 1024 * 1024).is_err());
		}
		let oversized = LiveDescriptor::Binding {
			expression: LiveExpr::Array(vec![LiveExpr::Array(Vec::new()); 100_001]),
			parameters: Object::new(),
		};
		assert!(encode(&oversized, 64 * 1024 * 1024).is_err());
		Ok(())
	}

	/// Fixed tags/flags retain literal names and reject malformed child frames.
	#[test]
	fn live_recipe_access_index_codec() -> FerrosGrynnResult<()> {
		for null_safe in [false, true] {
			let descriptor = LiveDescriptor::Binding {
				expression: LiveExpr::Access {
					base: Box::new(LiveExpr::Array(Vec::new())),
					field: "x".into(),
					null_safe,
				},
				parameters: Object::new(),
			};
			let mut golden = b"FGLD\x01\x00\x01\x00".to_vec();
			golden.extend_from_slice(&16_u64.to_le_bytes());
			golden.extend_from_slice(&20_u64.to_le_bytes());
			golden.extend_from_slice(&[0; 16]);
			golden.extend_from_slice(&[12, u8::from(null_safe)]);
			golden.extend_from_slice(&1_u64.to_le_bytes());
			golden.extend_from_slice(&[b'x', 8]);
			golden.extend_from_slice(&[0; 8]);
			assert_eq!(encode(&descriptor, 1024 * 1024)?, golden);
			assert_eq!(decode(&golden, 1024 * 1024)?, descriptor);
			let mut bad = golden.clone();
			bad[41] = 2;
			assert!(decode(&bad, 1024 * 1024).is_err());
			bad = golden.clone();
			bad[42..50].copy_from_slice(&u64::MAX.to_le_bytes());
			assert!(decode(&bad, 1024 * 1024).is_err());
			assert!(decode(&golden, golden.len()).is_err());
			for length in 0..golden.len() {
				assert!(decode(&golden[..length], 1024 * 1024).is_err());
			}
		}
		let descriptor = LiveDescriptor::Binding {
			expression: LiveExpr::Index {
				base: Box::new(LiveExpr::Array(Vec::new())),
				index: Box::new(LiveExpr::Parameter("i".into())),
			},
			parameters: Object::new(),
		};
		let mut golden = b"FGLD\x01\x00\x01\x00".to_vec();
		golden.extend_from_slice(&16_u64.to_le_bytes());
		golden.extend_from_slice(&20_u64.to_le_bytes());
		golden.extend_from_slice(&[0; 16]);
		golden.extend_from_slice(&[13, 8, 0, 0, 0, 0, 2]);
		golden.extend_from_slice(&1_u64.to_le_bytes());
		golden.extend_from_slice(&[b'i', 0, 0, 0, 0]);
		assert_eq!(encode(&descriptor, 1024 * 1024)?, golden);
		assert_eq!(decode(&golden, 1024 * 1024)?, descriptor);
		Ok(())
	}

	/// IF child order is fixed and every child is decoded under the same cap.
	#[test]
	fn live_recipe_conditional_codec() -> FerrosGrynnResult<()> {
		let descriptor = LiveDescriptor::Binding {
			expression: LiveExpr::Conditional {
				condition: Box::new(LiveExpr::Parameter("c".into())),
				then_value: Box::new(LiveExpr::Array(Vec::new())),
				else_value: Box::new(LiveExpr::Object(Vec::new())),
			},
			parameters: Object::new(),
		};
		let mut golden = b"FGLD\x01\x00\x01\x00".to_vec();
		golden.extend_from_slice(&16_u64.to_le_bytes());
		golden.extend_from_slice(&25_u64.to_le_bytes());
		golden.extend_from_slice(&[0; 16]);
		golden.extend_from_slice(&[14, 2]);
		golden.extend_from_slice(&1_u64.to_le_bytes());
		golden.extend_from_slice(&[b'c', 8, 0, 0, 0, 0, 9, 0, 0, 0, 0]);
		golden.extend_from_slice(&[0; 4]);
		assert_eq!(encode(&descriptor, 1024 * 1024)?, golden);
		assert_eq!(decode(&golden, 1024 * 1024)?, descriptor);
		assert_eq!(encode(&decode(&golden, 1024 * 1024)?, 1024 * 1024)?, golden);
		assert!(decode(&golden, golden.len()).is_err());
		for length in 0..golden.len() {
			assert!(decode(&golden[..length], 1024 * 1024).is_err());
		}
		let mut bad = golden.clone();
		bad[41] = u8::MAX;
		assert!(decode(&bad, 1024 * 1024).is_err());
		bad = golden;
		bad[52..56].copy_from_slice(&u32::MAX.to_le_bytes());
		assert!(decode(&bad, 1024 * 1024).is_err());
		Ok(())
	}

	/// Recipe bytes retain input order and duplicates before runtime Set
	/// construction.
	#[test]
	fn live_recipe_set_codec() -> FerrosGrynnResult<()> {
		let descriptor = LiveDescriptor::Binding {
			expression: LiveExpr::Set(vec![
				LiveExpr::Parameter("a".into()),
				LiveExpr::Parameter("b".into()),
				LiveExpr::Parameter("a".into()),
			]),
			parameters: Object::new(),
		};
		let mut golden = b"FGLD\x01\x00\x01\x00".to_vec();
		golden.extend_from_slice(&16_u64.to_le_bytes());
		golden.extend_from_slice(&39_u64.to_le_bytes());
		golden.extend_from_slice(&[0; 16]);
		golden.push(15);
		golden.extend_from_slice(&3_u32.to_le_bytes());
		for name in [b'a', b'b', b'a'] {
			golden.push(2);
			golden.extend_from_slice(&1_u64.to_le_bytes());
			golden.push(name);
		}
		golden.extend_from_slice(&[0; 4]);
		assert_eq!(encode(&descriptor, 1024 * 1024)?, golden);
		assert_eq!(decode(&golden, 1024 * 1024)?, descriptor);
		assert_eq!(encode(&decode(&golden, 1024 * 1024)?, 1024 * 1024)?, golden);
		assert!(decode(&golden, golden.len()).is_err());
		for length in 0..golden.len() {
			assert!(decode(&golden[..length], 1024 * 1024).is_err());
		}
		let mut bad = golden.clone();
		bad[45] = u8::MAX;
		assert!(decode(&bad, 1024 * 1024).is_err());
		bad = golden;
		bad[41..45].copy_from_slice(&u32::MAX.to_le_bytes());
		assert!(decode(&bad, 1024 * 1024).is_err());
		Ok(())
	}

	/// CASE bytes distinguish every ordered branch and checked optional child.
	#[test]
	fn live_recipe_case_codec() -> FerrosGrynnResult<()> {
		let descriptor = LiveDescriptor::Binding {
			expression: LiveExpr::Case {
				operand: None,
				branches: vec![
					(LiveExpr::Parameter("a".into()), LiveExpr::Array(Vec::new())),
					(LiveExpr::Parameter("b".into()), LiveExpr::Object(Vec::new())),
				],
				else_value: Some(Box::new(LiveExpr::Parameter("c".into()))),
			},
			parameters: Object::new(),
		};
		let mut golden = b"FGLD\x01\x00\x01\x00".to_vec();
		golden.extend_from_slice(&16_u64.to_le_bytes());
		golden.extend_from_slice(&51_u64.to_le_bytes());
		golden.extend_from_slice(&[0; 16]);
		golden.extend_from_slice(&[16, 0]);
		golden.extend_from_slice(&2_u32.to_le_bytes());
		for (name, tag) in [(b'a', 8), (b'b', 9)] {
			golden.push(2);
			golden.extend_from_slice(&1_u64.to_le_bytes());
			golden.extend_from_slice(&[name, tag, 0, 0, 0, 0]);
		}
		golden.extend_from_slice(&[1, 2]);
		golden.extend_from_slice(&1_u64.to_le_bytes());
		golden.push(b'c');
		golden.extend_from_slice(&[0; 4]);
		assert_eq!(encode(&descriptor, 1024 * 1024)?, golden);
		assert_eq!(decode(&golden, 1024 * 1024)?, descriptor);
		assert_eq!(encode(&decode(&golden, 1024 * 1024)?, 1024 * 1024)?, golden);
		assert!(decode(&golden, golden.len()).is_err());
		for length in 0..golden.len() {
			assert!(decode(&golden[..length], 1024 * 1024).is_err());
		}
		for (offset, value) in [(41, 2), (76, 2), (46, u8::MAX)] {
			let mut bad = golden.clone();
			bad[offset] = value;
			assert!(decode(&bad, 1024 * 1024).is_err());
		}
		for count in [0_u32, u32::MAX] {
			let mut bad = golden.clone();
			bad[42..46].copy_from_slice(&count.to_le_bytes());
			assert!(decode(&bad, 1024 * 1024).is_err());
		}
		let simple = LiveDescriptor::Binding {
			expression: LiveExpr::Case {
				operand: Some(Box::new(LiveExpr::Parameter("v".into()))),
				branches: vec![(LiveExpr::Parameter("a".into()), LiveExpr::Array(Vec::new()))],
				else_value: None,
			},
			parameters: Object::new(),
		};
		assert_eq!(decode(&encode(&simple, 1024 * 1024)?, 1024 * 1024)?, simple);
		let empty = LiveDescriptor::Binding {
			expression: LiveExpr::Case {
				operand: None,
				branches: Vec::new(),
				else_value: None,
			},
			parameters: Object::new(),
		};
		assert!(encode(&empty, 1024 * 1024).is_err());
		Ok(())
	}

	/// Explicit CAST framing checks target bytes before copying or parsing.
	#[test]
	fn live_recipe_cast_codec() -> FerrosGrynnResult<()> {
		let descriptor = LiveDescriptor::Binding {
			expression: LiveExpr::Cast {
				target: "int".into(),
				value: Box::new(LiveExpr::Parameter("x".into())),
			},
			parameters: Object::new(),
		};
		let mut golden = b"FGLD\x01\x00\x01\x00".to_vec();
		golden.extend_from_slice(&16_u64.to_le_bytes());
		golden.extend_from_slice(&26_u64.to_le_bytes());
		golden.extend_from_slice(&[0; 16]);
		golden.push(17);
		golden.extend_from_slice(&3_u64.to_le_bytes());
		golden.extend_from_slice(b"int");
		golden.push(2);
		golden.extend_from_slice(&1_u64.to_le_bytes());
		golden.push(b'x');
		golden.extend_from_slice(&[0; 4]);
		assert_eq!(encode(&descriptor, 1024 * 1024)?, golden);
		assert_eq!(decode(&golden, 1024 * 1024)?, descriptor);
		assert_eq!(encode(&decode(&golden, 1024 * 1024)?, 1024 * 1024)?, golden);
		assert!(decode(&golden, golden.len()).is_err());
		for length in 0..golden.len() {
			assert!(decode(&golden[..length], 1024 * 1024).is_err());
		}
		for (offset, value) in [(49, b'<'), (49, u8::MAX), (52, u8::MAX)] {
			let mut bad = golden.clone();
			bad[offset] = value;
			assert!(decode(&bad, 1024 * 1024).is_err());
		}
		let oversized = MAX_NODES + 1;
		let mut bad = golden[..40].to_vec();
		bad[16..24].copy_from_slice(&((oversized + 23) as u64).to_le_bytes());
		bad.push(17);
		bad.extend_from_slice(&(oversized as u64).to_le_bytes());
		bad.resize(bad.len() + oversized, u8::MAX);
		bad.extend_from_slice(&golden[52..]);
		let error = decode(&bad, 1024 * 1024)
			.err()
			.ok_or_else(|| invalid("oversized invalid UTF-8 cast target admitted"))?;
		assert!(
			error.cause().is_some_and(|cause| cause.to_string().contains("node count")),
			"{error:?}"
		);
		for count in [0_u64, u64::MAX] {
			let mut bad = golden.clone();
			bad[41..49].copy_from_slice(&count.to_le_bytes());
			assert!(decode(&bad, 1024 * 1024).is_err());
		}
		for target in [
			String::new(),
			"int>".to_owned(),
			format!("{}int{}", "option<".repeat(65), ">".repeat(65)),
			"x".repeat(128 * 1024),
		] {
			let bad = LiveDescriptor::Binding {
				expression: LiveExpr::Cast {
					target: target.into(),
					value: Box::new(LiveExpr::Parameter("x".into())),
				},
				parameters: Object::new(),
			};
			assert!(encode(&bad, 1024 * 1024).is_err());
		}
		Ok(())
	}

	/// Scalar operator tags and synthetic truthiness have fixed logical bytes.
	#[test]
	fn live_recipe_scalar_operator_codec() -> FerrosGrynnResult<()> {
		for (offset, operator) in [
			LiveBinaryOperator::Power,
			LiveBinaryOperator::Remainder,
			LiveBinaryOperator::Is,
			LiveBinaryOperator::NotIs,
			LiveBinaryOperator::NotIn,
			LiveBinaryOperator::AllIn,
			LiveBinaryOperator::AnyIn,
			LiveBinaryOperator::NoneIn,
			LiveBinaryOperator::AnyEqual,
			LiveBinaryOperator::AllEqual,
			LiveBinaryOperator::Contains,
			LiveBinaryOperator::ContainsNot,
			LiveBinaryOperator::ContainsAny,
			LiveBinaryOperator::ContainsAll,
			LiveBinaryOperator::Elvis,
			LiveBinaryOperator::Inside,
			LiveBinaryOperator::NotInside,
			LiveBinaryOperator::AllInside,
			LiveBinaryOperator::AnyInside,
			LiveBinaryOperator::NoneInside,
			LiveBinaryOperator::Outside,
			LiveBinaryOperator::Intersects,
		]
		.into_iter()
		.enumerate()
		{
			let descriptor = LiveDescriptor::Binding {
				expression: LiveExpr::Parameter("x".into())
					.binary(operator, LiveExpr::Parameter("y".into())),
				parameters: Object::new(),
			};
			let mut golden = b"FGLD\x01\x00\x01\x00".to_vec();
			golden.extend_from_slice(&16_u64.to_le_bytes());
			golden.extend_from_slice(&26_u64.to_le_bytes());
			golden.extend_from_slice(&[0; 16]);
			golden.extend_from_slice(&[5, 14 + offset as u8]);
			for name in [b'x', b'y'] {
				golden.push(2);
				golden.extend_from_slice(&1_u64.to_le_bytes());
				golden.push(name);
			}
			golden.extend_from_slice(&[0; 4]);
			assert_eq!(encode(&descriptor, 1024 * 1024)?, golden);
			assert_eq!(decode(&golden, 1024 * 1024)?, descriptor);
			assert!(decode(&golden, golden.len()).is_err());
			for tag in [36, u8::MAX] {
				let mut bad = golden.clone();
				bad[41] = tag;
				assert!(decode(&bad, 1024 * 1024).is_err());
			}
			for length in 40..golden.len() {
				assert!(decode(&golden[..length], 1024 * 1024).is_err());
			}
		}
		let descriptor = LiveDescriptor::Binding {
			expression: LiveExpr::Parameter("x".into()).truthy(),
			parameters: Object::new(),
		};
		let bytes = encode(&descriptor, 1024 * 1024)?;
		assert_eq!(&bytes[40..43], &[4, 4, 2]);
		assert_eq!(decode(&bytes, 1024 * 1024)?, descriptor);
		Ok(())
	}

	/// Modified calls retain ordered names and checked type/DISTINCT flags;
	/// the original positional call keeps its existing bytes.
	#[test]
	fn live_recipe_call_modifiers_codec() -> FerrosGrynnResult<()> {
		for modified in [false, true] {
			let expression = if modified {
				LiveExpr::CallWithOptions {
					name: "f".into(),
					type_argument: Some("int".into()),
					distinct: true,
					arguments: vec![(Some("a".into()), LiveExpr::Parameter("x".into()))],
				}
			} else {
				LiveExpr::Call {
					name: "f".into(),
					arguments: vec![LiveExpr::Parameter("x".into())],
				}
			};
			let descriptor = LiveDescriptor::Binding {
				expression,
				parameters: Object::new(),
			};
			let mut body = vec![if modified {
				19
			} else {
				6
			}];
			body.extend_from_slice(&1_u64.to_le_bytes());
			body.push(b'f');
			if modified {
				body.push(3);
				body.extend_from_slice(&3_u64.to_le_bytes());
				body.extend_from_slice(b"int");
			}
			body.extend_from_slice(&1_u32.to_le_bytes());
			if modified {
				body.push(1);
				body.extend_from_slice(&1_u64.to_le_bytes());
				body.push(b'a');
			}
			body.push(2);
			body.extend_from_slice(&1_u64.to_le_bytes());
			body.push(b'x');
			body.extend_from_slice(&[0; 4]);
			let mut golden = b"FGLD\x01\x00\x01\x00".to_vec();
			golden.extend_from_slice(&16_u64.to_le_bytes());
			golden.extend_from_slice(&(body.len() as u64).to_le_bytes());
			golden.extend_from_slice(&[0; 16]);
			golden.extend_from_slice(&body);
			assert_eq!(encode(&descriptor, 1024 * 1024)?, golden);
			assert_eq!(decode(&golden, 1024 * 1024)?, descriptor);
			assert!(decode(&golden, golden.len()).is_err());
			for length in 0..golden.len() {
				assert!(decode(&golden[..length], 1024 * 1024).is_err());
			}
			if modified {
				for (offset, value) in [(50, 4), (59, u8::MAX), (66, 2), (75, u8::MAX)] {
					let mut bad = golden.clone();
					bad[offset] = value;
					assert!(decode(&bad, 1024 * 1024).is_err(), "offset {offset}");
				}
				let mut bad = golden.clone();
				bad[62..66].copy_from_slice(&u32::MAX.to_le_bytes());
				assert!(decode(&bad, 1024 * 1024).is_err());
			}
		}
		for flags in 0_u8..4 {
			let descriptor = LiveDescriptor::Binding {
				expression: LiveExpr::CallWithOptions {
					name: "f".into(),
					type_argument: (flags & 1 != 0).then(|| "option<array<int>>".into()),
					distinct: flags & 2 != 0,
					arguments: vec![(None, LiveExpr::Parameter("x".into()))],
				},
				parameters: Object::new(),
			};
			assert_eq!(decode(&encode(&descriptor, 1024 * 1024)?, 1024 * 1024)?, descriptor);
		}
		for (name, target) in [
			("", "int".to_owned()),
			("a", String::new()),
			("a", format!("{}int{}", "array<".repeat(65), ">".repeat(65))),
		] {
			let descriptor = LiveDescriptor::Binding {
				expression: LiveExpr::CallWithOptions {
					name: "f".into(),
					type_argument: Some(target.into()),
					distinct: false,
					arguments: vec![(Some(name.into()), LiveExpr::Parameter("x".into()))],
				},
				parameters: Object::new(),
			};
			assert!(encode(&descriptor, 1024 * 1024).is_err());
		}
		Ok(())
	}

	/// Range flags and optional children have one checked durable encoding.
	#[test]
	fn live_recipe_range_codec() -> FerrosGrynnResult<()> {
		let descriptor = LiveDescriptor::Binding {
			expression: LiveExpr::Range {
				start: Some(Box::new(LiveExpr::Parameter("x".into()))),
				end: Some(Box::new(LiveExpr::Parameter("y".into()))),
				inclusive_end: true,
			},
			parameters: Object::new(),
		};
		let mut golden = b"FGLD\x01\x00\x01\x00".to_vec();
		golden.extend_from_slice(&16_u64.to_le_bytes());
		golden.extend_from_slice(&26_u64.to_le_bytes());
		golden.extend_from_slice(&[0; 16]);
		golden.extend_from_slice(&[18, 7]);
		for name in [b'x', b'y'] {
			golden.push(2);
			golden.extend_from_slice(&1_u64.to_le_bytes());
			golden.push(name);
		}
		golden.extend_from_slice(&[0; 4]);
		assert_eq!(encode(&descriptor, 1024 * 1024)?, golden);
		assert_eq!(decode(&golden, 1024 * 1024)?, descriptor);
		assert_eq!(encode(&decode(&golden, 1024 * 1024)?, 1024 * 1024)?, golden);
		assert!(decode(&golden, golden.len()).is_err());
		for length in 0..golden.len() {
			assert!(decode(&golden[..length], 1024 * 1024).is_err());
		}
		for (offset, value) in [(41, 8), (41, u8::MAX), (42, u8::MAX), (52, u8::MAX)] {
			let mut bad = golden.clone();
			bad[offset] = value;
			assert!(decode(&bad, 1024 * 1024).is_err());
		}
		for flags in 0_u8..8 {
			let optional = LiveDescriptor::Binding {
				expression: LiveExpr::Range {
					start: (flags & 1 != 0).then(|| Box::new(LiveExpr::Parameter("x".into()))),
					end: (flags & 2 != 0).then(|| Box::new(LiveExpr::Parameter("y".into()))),
					inclusive_end: flags & 4 != 0,
				},
				parameters: Object::new(),
			};
			assert_eq!(decode(&encode(&optional, 1024 * 1024)?, 1024 * 1024)?, optional);
		}
		let mut deep = LiveExpr::Parameter("x".into());
		for _ in 0..66 {
			deep = LiveExpr::Range {
				start: Some(Box::new(deep)),
				end: None,
				inclusive_end: false,
			};
		}
		assert!(
			encode(
				&LiveDescriptor::Binding {
					expression: deep,
					parameters: Object::new()
				},
				1024 * 1024
			)
			.is_err()
		);
		Ok(())
	}

	#[test]
	fn descriptor_limits_reject_runtime_captures_and_deep_expressions() -> FerrosGrynnResult<()> {
		let regex = crate::values::Regex::new("a+").map_err(|error| invalid(&error.to_string()))?;
		let runtime_capture = LiveDescriptor::Binding {
			expression: LiveExpr::Literal(Value::Regex(regex)),
			parameters: Object::new(),
		};
		assert!(encode(&runtime_capture, 1024 * 1024).is_err());
		let mut expression = LiveExpr::Literal(Value::Null);
		for _ in 0..66 {
			expression = LiveExpr::Not(Box::new(expression));
		}
		assert!(
			encode(
				&LiveDescriptor::Binding {
					expression,
					parameters: Object::new()
				},
				1024 * 1024
			)
			.is_err()
		);
		let invalid_fields = LiveQuery::new(LiveSource::Record("person:a".parse()?))
			.with_projection(LiveProjection::Fields(vec![]));
		assert!(encode(&LiveDescriptor::Query(invalid_fields), 1024 * 1024).is_err());
		let invalid_parameter = LiveDescriptor::Binding {
			expression: LiveExpr::Parameter("".into()),
			parameters: Object::new(),
		};
		assert!(encode(&invalid_parameter, 1024 * 1024).is_err());
		Ok(())
	}

	#[test]
	fn descriptor_rejects_unused_historical_dictionary_entries() -> FerrosGrynnResult<()> {
		let descriptor = LiveDescriptor::Binding {
			expression: LiveExpr::Literal(Value::Null),
			parameters: Object::new(),
		};
		let canonical = encode(&descriptor, 1024 * 1024)?;
		let dictionary_length = u64::from_le_bytes(
			canonical[8..16].try_into().map_err(|_error| invalid("test dictionary length"))?,
		) as usize;
		let mut dictionary = Vec::new();
		dictionary.extend_from_slice(&0_u32.to_le_bytes());
		dictionary.extend_from_slice(&1_u32.to_le_bytes());
		dictionary.extend_from_slice(&1_u64.to_le_bytes());
		dictionary.push(b'x');
		dictionary.extend_from_slice(&[0; 8]);
		let mut malformed = canonical[..HEADER_BYTES].to_vec();
		malformed[8..16].copy_from_slice(&(dictionary.len() as u64).to_le_bytes());
		malformed.extend_from_slice(&dictionary);
		malformed.extend_from_slice(&canonical[HEADER_BYTES + dictionary_length..]);
		assert!(decode(&malformed, 1024 * 1024).is_err());
		assert_eq!(decode(&canonical, 1024 * 1024)?, descriptor);
		Ok(())
	}
}

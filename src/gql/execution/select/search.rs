//! Exact Fulltext and KNN fallback over snapshot-visible SELECT rows.

use std::{
	cmp::Ordering,
	collections::{BTreeMap, BTreeSet, BinaryHeap},
};

use super::*;
use crate::gql::ir::{LogicalFulltextDefinition, LogicalVectorDefinition, NamedFunctionKey};

pub(super) struct PreparedSearch {
	fulltext: BTreeMap<u32, PreparedFulltext>,
	knn: Option<PreparedKnn>,
	physical_fulltext: Option<u32>,
	physical_knn: Option<u32>,
}

struct PreparedFulltext {
	terms: BTreeSet<Box<str>>,
	definition: LogicalFulltextDefinition,
}

struct PreparedKnn {
	predicate: u32,
	k: usize,
	ef: Option<usize>,
	query: crate::values::Vector,
	distance: RuntimeDistance,
	definition: Option<LogicalVectorDefinition>,
}

pub(super) fn contains_search(expression: &TypedExpr) -> bool {
	match &expression.kind {
		TypedExprKind::Fulltext {
			..
		}
		| TypedExprKind::Knn {
			..
		} => true,
		TypedExprKind::Array(values) | TypedExprKind::Set(values) => {
			values.iter().any(contains_search)
		}
		TypedExprKind::Object(fields) => fields.iter().any(|(_, value)| contains_search(value)),
		TypedExprKind::Range {
			start,
			end,
			..
		} => {
			start.as_deref().is_some_and(contains_search)
				|| end.as_deref().is_some_and(contains_search)
		}
		TypedExprKind::Unary {
			expr,
			..
		}
		| TypedExprKind::Field {
			base: expr,
			..
		}
		| TypedExprKind::Destructure {
			base: expr,
			..
		}
		| TypedExprKind::Cast {
			expr,
			..
		} => contains_search(expr),
		TypedExprKind::Binary {
			left,
			right,
			..
		}
		| TypedExprKind::Index {
			base: left,
			index: right,
		}
		| TypedExprKind::Filter {
			base: left,
			predicate: right,
		} => contains_search(left) || contains_search(right),
		TypedExprKind::Conditional {
			condition,
			then_value,
			else_value,
		} => {
			contains_search(condition) || contains_search(then_value) || contains_search(else_value)
		}
		TypedExprKind::Case {
			operand,
			branches,
			else_value,
		} => {
			operand.as_deref().is_some_and(contains_search)
				|| branches
					.iter()
					.any(|(condition, value)| contains_search(condition) || contains_search(value))
				|| else_value.as_deref().is_some_and(contains_search)
		}
		TypedExprKind::Builtin {
			arguments,
			..
		}
		| TypedExprKind::NamedInvoke {
			arguments,
			..
		} => arguments.iter().any(contains_search),
		TypedExprKind::Invoke {
			callee,
			arguments,
		} => contains_search(callee) || arguments.iter().any(contains_search),
		TypedExprKind::Aggregate {
			argument,
			..
		} => argument.as_deref().is_some_and(contains_search),
		TypedExprKind::Constant(_)
		| TypedExprKind::Slot(_)
		| TypedExprKind::Column(_)
		| TypedExprKind::CurrentField(_)
		| TypedExprKind::Closure(_)
		| TypedExprKind::Traversal(_)
		| TypedExprKind::Subquery {
			..
		} => false,
	}
}

pub(super) fn prepare_search_inputs(
	filter: &TypedExpr,
	access: &LogicalSelectAccess,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<PreparedSearch> {
	let mut fulltext = Vec::new();
	let mut knn = None;
	collect_search_predicates(filter, true, &mut fulltext, &mut knn)?;
	let mut public_slots = BTreeSet::new();
	let mut prepared_fulltext = BTreeMap::new();
	for expression in fulltext {
		let TypedExprKind::Fulltext {
			op,
			right,
			definitions,
			..
		} = &expression.kind
		else {
			unreachable!("Fulltext collection retained a non-Fulltext expression")
		};
		if let crate::syn::FulltextOperator::Slot(slot) = op
			&& !public_slots.insert(*slot)
		{
			return Err(FerrosGrynnError::invalid_argument(format!(
				"Fulltext match slot {slot} is duplicated in one statement"
			)));
		}
		let definition = choose_fulltext_definition(definitions, access)?;
		prepared_fulltext.insert(expression.span.offset, PreparedFulltext {
			terms: fulltext_query_terms(*op, right, definition, slots, source, control)?,
			definition: definition.clone(),
		});
	}
	let knn = knn
		.map(|expression| prepare_knn_input(expression, access, slots, source, control))
		.transpose()?;
	Ok(PreparedSearch {
		fulltext: prepared_fulltext,
		knn,
		physical_fulltext: None,
		physical_knn: None,
	})
}

/// Uses one positive Fulltext conjunct as a snapshot-exact source access path.
///
/// The returned records already carry the exact BM25/highlight outcome, so the
/// later expression pass only evaluates residual predicates. Unsupported query
/// shapes and absent automatic bindings return `None` and retain the scan
/// oracle.
#[allow(clippy::too_many_arguments, reason = "physical Fulltext needs the SELECT source context")]
pub(super) fn try_physical_fulltext<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	filter: &TypedExpr,
	access: &LogicalSelectAccess,
	planned_index: Option<crate::datastore::IndexId>,
	class: &str,
	graph: Option<&GraphRef>,
	prepared: &mut PreparedSearch,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Option<Vec<(RecordId, FulltextMatch)>>> {
	if matches!(access, LogicalSelectAccess::NoIndex) || prepared.knn.is_some() {
		return Ok(None);
	}
	let mut fulltext = Vec::new();
	let mut knn = None;
	collect_search_predicates(filter, true, &mut fulltext, &mut knn)?;
	let [expression] = fulltext.as_slice() else {
		return Ok(None);
	};
	if !is_positive_conjunct(filter, expression.span.offset) {
		return Ok(None);
	}
	let TypedExprKind::Fulltext {
		op,
		definitions: _,
		..
	} = &expression.kind
	else {
		unreachable!("Fulltext collection retained a non-Fulltext expression")
	};
	let prepared_fulltext = prepared.fulltext.get(&expression.span.offset).ok_or_else(|| {
		FerrosGrynnError::invalid_argument("Fulltext query was not prepared before source access")
	})?;
	let definition = &prepared_fulltext.definition;
	if !definition.class.eq_ignore_ascii_case(class)
		|| definition.state != crate::datastore::ClassIndexState::Ready
		|| planned_index != Some(definition.index)
	{
		return Ok(None);
	}
	let forced = matches!(access, LogicalSelectAccess::Indexes(hints) if hints.iter().any(|hint| {
		hint.index.as_ref() == definition.name.as_str()
			&& hint.source.as_deref().is_none_or(|source| {
				source.eq_ignore_ascii_case(definition.source.as_ref())
			})
	}));
	let batch = match read.fulltext_index(
		snapshot,
		class,
		graph,
		definition.index,
		&prepared_fulltext.terms,
	) {
		Ok(Some(batch)) => batch,
		Ok(None) if forced => {
			return Err(FerrosGrynnError::invalid_argument(format!(
				"forced Fulltext index `{}` has no compatible physical binding",
				definition.name
			)));
		}
		Ok(None) => return Ok(None),
		Err(error) if forced => return Err(error),
		// An automatic secondary path is never required for correctness. Its
		// checksum or structural failure falls back to the exact analyzer scan.
		Err(_error) => return Ok(None),
	};
	let average_length = if batch.document_count == 0 {
		0.0
	} else {
		batch.total_document_length as f64 / batch.document_count as f64
	};
	let mut output = Vec::with_capacity(batch.documents.len());
	for document in batch.documents {
		control.check()?;
		let matched = !prepared_fulltext.terms.is_empty()
			&& match op {
				crate::syn::FulltextOperator::All => {
					prepared_fulltext.terms.iter().all(|term| document.terms.contains_key(term))
				}
				crate::syn::FulltextOperator::Any
				| crate::syn::FulltextOperator::Slot(_)
				| crate::syn::FulltextOperator::AnyOf => !document.terms.is_empty(),
			};
		if !matched {
			continue;
		}
		let frequencies = document
			.terms
			.iter()
			.map(|(term, value)| (term.clone(), u64::from(value.term_frequency)))
			.collect::<BTreeMap<_, _>>();
		let document_frequencies = document
			.terms
			.iter()
			.map(|(term, value)| (term.clone(), value.document_frequency))
			.collect::<BTreeMap<_, _>>();
		let score = bm25_score(
			&frequencies,
			&document_frequencies,
			batch.document_count,
			u64::from(document.document_length),
			average_length,
			definition.k1,
			definition.b,
		);
		let mut spans = if definition.highlights {
			document
				.terms
				.values()
				.flat_map(|value| value.spans.iter().copied())
				.collect::<Vec<_>>()
		} else {
			Vec::new()
		};
		spans.sort_unstable();
		merge_spans(&mut spans);
		let text = match read.record_field(
			snapshot,
			&document.record_id,
			graph,
			definition.field.as_ref(),
		)? {
			Value::String(value) => value.as_str().into(),
			Value::None | Value::Null => {
				return Err(FerrosGrynnError::invalid_storage_file(
					"Fulltext posting references a document without its indexed string",
				));
			}
			_ => {
				return Err(FerrosGrynnError::invalid_storage_file(
					"Fulltext posting references a non-string indexed value",
				));
			}
		};
		output.push((document.record_id, FulltextMatch {
			predicate: expression.span.offset,
			slot: match op {
				crate::syn::FulltextOperator::Slot(slot) => Some(*slot),
				_ => None,
			},
			matched: true,
			highlights: definition.highlights,
			score_bits: score.to_bits(),
			text,
			spans,
		}));
	}
	prepared.physical_fulltext = Some(expression.span.offset);
	Ok(Some(output))
}

/// Uses an exact-filter KNN predicate as a physical HNSW/DiskANN source path.
/// Residual predicates retain the exact prefiltered scan required by the
/// language contract.
#[allow(clippy::too_many_arguments, reason = "physical KNN needs the SELECT source context")]
pub(super) fn try_physical_knn<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	filter: &TypedExpr,
	access: &LogicalSelectAccess,
	planned_index: Option<crate::datastore::IndexId>,
	class: &str,
	graph: Option<&GraphRef>,
	prepared: &mut PreparedSearch,
) -> FerrosGrynnResult<Option<Vec<(RecordId, KnnMatch)>>> {
	if matches!(access, LogicalSelectAccess::NoIndex)
		|| !matches!(filter.kind, TypedExprKind::Knn { .. })
	{
		return Ok(None);
	}
	let prepared_knn = prepared.knn.as_ref().ok_or_else(|| {
		FerrosGrynnError::invalid_argument("KNN query was not prepared before source access")
	})?;
	let Some(definition) = prepared_knn.definition.as_ref() else {
		return Ok(None);
	};
	if !definition.class.eq_ignore_ascii_case(class)
		|| definition.state != crate::datastore::ClassIndexState::Ready
		|| planned_index != Some(definition.index)
	{
		return Ok(None);
	}
	let forced = matches!(access, LogicalSelectAccess::Indexes(hints) if hints.iter().any(|hint| {
		hint.index.as_ref() == definition.name.as_str()
			&& hint.source.as_deref().is_none_or(|source| {
				source.eq_ignore_ascii_case(definition.source.as_ref())
			})
	}));
	let required = forced || prepared_knn.ef.is_some();
	let breadth = prepared_knn.ef.unwrap_or_else(|| match definition.options {
		crate::datastore::ClassIndexOptions::Hnsw(options) => {
			options.default_search_breadth(prepared_knn.k)
		}
		_ => prepared_knn.k.max(64),
	});
	let hits = match read.vector_index(
		snapshot,
		class,
		graph,
		definition.index,
		&prepared_knn.query,
		prepared_knn.k,
		breadth,
	) {
		Ok(Some(hits)) => hits,
		Ok(None) if required => {
			return Err(FerrosGrynnError::invalid_argument(format!(
				"required ANN index `{}` has no compatible physical binding",
				definition.name
			)));
		}
		Ok(None) => return Ok(None),
		Err(error) if required => return Err(error),
		// Automatic ANN is an acceleration only. Corrupt or unavailable side
		// files must never suppress the exact scan oracle.
		Err(_error) => return Ok(None),
	};
	if hits.len() > prepared_knn.k {
		return Err(FerrosGrynnError::invalid_storage_file(
			"ANN index returned more than K candidates",
		));
	}
	prepared.physical_knn = Some(prepared_knn.predicate);
	Ok(Some(
		hits.into_iter()
			.map(|hit| {
				(hit.record_id, KnnMatch {
					predicate: prepared_knn.predicate,
					distance_bits: hit.distance_bits,
				})
			})
			.collect(),
	))
}

fn is_positive_conjunct(expression: &TypedExpr, predicate: u32) -> bool {
	if expression.span.offset == predicate
		&& matches!(expression.kind, TypedExprKind::Fulltext { .. })
	{
		return true;
	}
	match &expression.kind {
		TypedExprKind::Binary {
			left,
			op: crate::syn::BinaryOperator::And,
			right,
		} => is_positive_conjunct(left, predicate) || is_positive_conjunct(right, predicate),
		_ => false,
	}
}

pub(super) fn prepare_search_rows<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	mut rows: RowSet,
	filter: &TypedExpr,
	prepared: &PreparedSearch,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<RowSet> {
	let mut fulltext = Vec::new();
	let mut knn = None;
	collect_search_predicates(filter, true, &mut fulltext, &mut knn)?;
	for expression in fulltext {
		if prepared.physical_fulltext == Some(expression.span.offset) {
			continue;
		}
		let TypedExprKind::Fulltext {
			left,
			op,
			definitions: _,
			..
		} = &expression.kind
		else {
			unreachable!("Fulltext collection retained a non-Fulltext expression")
		};
		let prepared = prepared.fulltext.get(&expression.span.offset).ok_or_else(|| {
			FerrosGrynnError::invalid_argument("Fulltext query was not prepared before row pull")
		})?;
		rows = prepare_fulltext(
			read,
			snapshot,
			rows,
			expression.span.offset,
			left,
			*op,
			&prepared.terms,
			&prepared.definition,
			slots,
			source,
			control,
		)?;
	}
	if let Some(expression) = knn
		&& prepared.physical_knn != Some(expression.span.offset)
	{
		let prepared = prepared.knn.as_ref().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("KNN query was not prepared before row pull")
		})?;
		rows = prepare_knn(
			read, snapshot, rows, filter, expression, prepared, slots, source, control,
		)?;
	}
	Ok(rows)
}

fn collect_search_predicates<'a>(
	expression: &'a TypedExpr,
	positive_conjunction: bool,
	fulltext: &mut Vec<&'a TypedExpr>,
	knn: &mut Option<&'a TypedExpr>,
) -> FerrosGrynnResult<()> {
	match &expression.kind {
		TypedExprKind::Fulltext {
			..
		} => fulltext.push(expression),
		TypedExprKind::Knn {
			..
		} => {
			if !positive_conjunction {
				return Err(FerrosGrynnError::invalid_argument(
					"KNN must be a positive top-level conjunction in SELECT WHERE",
				));
			}
			if knn.replace(expression).is_some() {
				return Err(FerrosGrynnError::invalid_argument(
					"one SELECT source may contain only one KNN predicate",
				));
			}
		}
		TypedExprKind::Binary {
			left,
			op: crate::syn::BinaryOperator::And,
			right,
		} if positive_conjunction => {
			collect_search_predicates(left, true, fulltext, knn)?;
			collect_search_predicates(right, true, fulltext, knn)?;
		}
		_ if contains_search(expression) => {
			collect_nested_search(expression, fulltext, knn)?;
		}
		_ => {}
	}
	Ok(())
}

fn collect_nested_search<'a>(
	expression: &'a TypedExpr,
	fulltext: &mut Vec<&'a TypedExpr>,
	knn: &mut Option<&'a TypedExpr>,
) -> FerrosGrynnResult<()> {
	match &expression.kind {
		TypedExprKind::Fulltext {
			..
		} => fulltext.push(expression),
		TypedExprKind::Knn {
			..
		} => {
			return Err(FerrosGrynnError::invalid_argument(
				"KNN must be a positive top-level conjunction in SELECT WHERE",
			));
		}
		TypedExprKind::Binary {
			left,
			right,
			..
		}
		| TypedExprKind::Index {
			base: left,
			index: right,
		}
		| TypedExprKind::Filter {
			base: left,
			predicate: right,
		} => {
			collect_nested_search(left, fulltext, knn)?;
			collect_nested_search(right, fulltext, knn)?;
		}
		TypedExprKind::Unary {
			expr,
			..
		}
		| TypedExprKind::Field {
			base: expr,
			..
		}
		| TypedExprKind::Destructure {
			base: expr,
			..
		}
		| TypedExprKind::Cast {
			expr,
			..
		} => collect_nested_search(expr, fulltext, knn)?,
		TypedExprKind::Array(values) | TypedExprKind::Set(values) => {
			for value in values {
				collect_nested_search(value, fulltext, knn)?;
			}
		}
		TypedExprKind::Object(fields) => {
			for (_, value) in fields {
				collect_nested_search(value, fulltext, knn)?;
			}
		}
		TypedExprKind::Range {
			start,
			end,
			..
		} => {
			if let Some(value) = start {
				collect_nested_search(value, fulltext, knn)?;
			}
			if let Some(value) = end {
				collect_nested_search(value, fulltext, knn)?;
			}
		}
		TypedExprKind::Conditional {
			condition,
			then_value,
			else_value,
		} => {
			collect_nested_search(condition, fulltext, knn)?;
			collect_nested_search(then_value, fulltext, knn)?;
			collect_nested_search(else_value, fulltext, knn)?;
		}
		TypedExprKind::Case {
			operand,
			branches,
			else_value,
		} => {
			if let Some(value) = operand {
				collect_nested_search(value, fulltext, knn)?;
			}
			for (condition, value) in branches {
				collect_nested_search(condition, fulltext, knn)?;
				collect_nested_search(value, fulltext, knn)?;
			}
			if let Some(value) = else_value {
				collect_nested_search(value, fulltext, knn)?;
			}
		}
		TypedExprKind::Builtin {
			arguments,
			..
		}
		| TypedExprKind::NamedInvoke {
			arguments,
			..
		} => {
			for value in arguments {
				collect_nested_search(value, fulltext, knn)?;
			}
		}
		TypedExprKind::Invoke {
			callee,
			arguments,
		} => {
			collect_nested_search(callee, fulltext, knn)?;
			for value in arguments {
				collect_nested_search(value, fulltext, knn)?;
			}
		}
		TypedExprKind::Aggregate {
			argument,
			..
		} => {
			if let Some(value) = argument {
				collect_nested_search(value, fulltext, knn)?;
			}
		}
		TypedExprKind::Constant(_)
		| TypedExprKind::Slot(_)
		| TypedExprKind::Column(_)
		| TypedExprKind::CurrentField(_)
		| TypedExprKind::Closure(_)
		| TypedExprKind::Traversal(_)
		| TypedExprKind::Subquery {
			..
		} => {}
	}
	Ok(())
}

fn choose_fulltext_definition<'a>(
	definitions: &'a [LogicalFulltextDefinition],
	access: &LogicalSelectAccess,
) -> FerrosGrynnResult<&'a LogicalFulltextDefinition> {
	let forced = match access {
		LogicalSelectAccess::Indexes(hints) => definitions
			.iter()
			.filter(|definition| {
				hints.iter().any(|hint| {
					hint.index.as_ref() == definition.name.as_str()
						&& hint.source.as_deref().is_none_or(|source| {
							source.eq_ignore_ascii_case(definition.source.as_ref())
						})
				})
			})
			.collect::<Vec<_>>(),
		LogicalSelectAccess::Automatic | LogicalSelectAccess::NoIndex => Vec::new(),
	};
	if forced.len() > 1 {
		return Err(FerrosGrynnError::invalid_argument(
			"Fulltext WITH INDEX candidates select more than one definition",
		));
	}
	if let Some(definition) = forced.first().copied() {
		if definition.state != crate::datastore::ClassIndexState::Ready {
			return Err(FerrosGrynnError::invalid_argument(format!(
				"forced Fulltext index `{}` is not Ready",
				definition.name
			)));
		}
		return Ok(definition);
	}
	match definitions {
		[definition] => Ok(definition),
		_ => Err(FerrosGrynnError::invalid_argument(
			"multiple compatible Fulltext definitions require WITH INDEX",
		)),
	}
}

#[allow(clippy::too_many_arguments, reason = "exact Fulltext uses the complete SELECT context")]
fn prepare_fulltext<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	rows: RowSet,
	predicate: u32,
	left: &TypedExpr,
	op: crate::syn::FulltextOperator,
	terms: &BTreeSet<Box<str>>,
	definition: &LogicalFulltextDefinition,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<RowSet> {
	let mut document_count = 0_u64;
	let mut total_length = 0_u64;
	let mut frequencies = BTreeMap::<Box<str>, u64>::new();
	rows.visit(|row| {
		control.check()?;
		let value = eval_select_expr(read, snapshot, left, &row, None, slots, source, control)?;
		let Value::String(text) = value else {
			if matches!(value, Value::None | Value::Null) {
				return Ok(());
			}
			return Err(FerrosGrynnError::invalid_argument(
				"Fulltext field produced a non-string value",
			));
		};
		document_count = document_count.saturating_add(1);
		let tokens = analyze_text(definition, text.as_str(), control, source)?;
		total_length = total_length.saturating_add(tokens.len() as u64);
		let mut present = BTreeSet::new();
		for token in tokens {
			if terms.contains(token.term.as_str()) {
				present.insert(token.term.as_str().into());
			}
		}
		for term in present {
			*frequencies.entry(term).or_default() += 1;
		}
		Ok(())
	})?;
	let average_length = if document_count == 0 {
		0.0
	} else {
		total_length as f64 / document_count as f64
	};
	let mut output = RowCollector::new(read, control, false);
	rows.consume(|mut row| {
		control.check()?;
		let value = eval_select_expr(read, snapshot, left, &row, None, slots, source, control)?;
		let (matched, score, text, spans) = match value {
			Value::None | Value::Null => (false, 0.0, Box::<str>::from(""), Vec::new()),
			Value::String(text) => {
				let tokens = analyze_text(definition, text.as_str(), control, source)?;
				let mut term_frequency = BTreeMap::<Box<str>, u64>::new();
				let mut spans = Vec::new();
				for token in &tokens {
					if terms.contains(token.term.as_str()) {
						*term_frequency.entry(token.term.as_str().into()).or_default() += 1;
						if definition.highlights {
							spans.push((token.start, token.end));
						}
					}
				}
				let matched = !terms.is_empty()
					&& match op {
						crate::syn::FulltextOperator::All => {
							terms.iter().all(|term| term_frequency.contains_key(term))
						}
						crate::syn::FulltextOperator::Any
						| crate::syn::FulltextOperator::Slot(_)
						| crate::syn::FulltextOperator::AnyOf => !term_frequency.is_empty(),
					};
				let score = if matched {
					bm25_score(
						&term_frequency,
						&frequencies,
						document_count,
						tokens.len() as u64,
						average_length,
						definition.k1,
						definition.b,
					)
				} else {
					0.0
				};
				spans.sort_unstable();
				merge_spans(&mut spans);
				(matched, score, text.as_str().into(), spans)
			}
			_ => {
				return Err(FerrosGrynnError::invalid_argument(
					"Fulltext field produced a non-string value",
				));
			}
		};
		row.fulltext.push(FulltextMatch {
			predicate,
			slot: match op {
				crate::syn::FulltextOperator::Slot(slot) => Some(slot),
				_ => None,
			},
			matched,
			highlights: definition.highlights,
			score_bits: score.to_bits(),
			text,
			spans,
		});
		output.push(read, snapshot, row)
	})?;
	output.finish()
}

fn fulltext_query_terms(
	op: crate::syn::FulltextOperator,
	right: &TypedExpr,
	definition: &LogicalFulltextDefinition,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<BTreeSet<Box<str>>> {
	let query = eval(right, None, slots, source, None, Some(control))?;
	let inputs = match (op, query) {
		(
			crate::syn::FulltextOperator::Any | crate::syn::FulltextOperator::Slot(_),
			Value::String(v),
		) => {
			vec![v]
		}
		(
			crate::syn::FulltextOperator::All | crate::syn::FulltextOperator::AnyOf,
			Value::Array(v),
		) => v
			.into_iter()
			.map(|value| match value {
				Value::String(value) => Ok(value),
				_ => Err(FerrosGrynnError::invalid_argument(
					"Fulltext query arrays must contain only strings",
				)),
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?,
		_ => {
			return Err(FerrosGrynnError::invalid_argument(
				"Fulltext query has an invalid runtime type",
			));
		}
	};
	let mut terms = BTreeSet::<Box<str>>::new();
	for input in inputs {
		for token in analyze_text(definition, input.as_str(), control, source)? {
			terms.insert(token.term.as_str().into());
		}
	}
	Ok(terms)
}

fn analyze_text(
	definition: &LogicalFulltextDefinition,
	input: &str,
	control: &ExecutionControl,
	source: &str,
) -> FerrosGrynnResult<Vec<crate::datastore::AnalyzedToken>> {
	if let Some(function) = definition.analyzer_function {
		let plan = control.resolve_named_function(function)?;
		let _frame = control.enter_named_function(function)?;
		let result = invoke_logical_closure(control, &plan, &[], &[Value::from(input)], source)?;
		crate::datastore::analyze_function_result(&definition.analyzer, input, &result)
	} else {
		crate::datastore::analyze_builtin(&definition.analyzer, input)
	}
}

fn bm25_score(
	term_frequency: &BTreeMap<Box<str>, u64>,
	document_frequency: &BTreeMap<Box<str>, u64>,
	document_count: u64,
	document_length: u64,
	average_length: f64,
	k1: f64,
	b: f64,
) -> f64 {
	if document_count == 0 || average_length == 0.0 {
		return 0.0;
	}
	term_frequency.iter().fold(0.0, |score, (term, frequency)| {
		let document_frequency = document_frequency.get(term).copied().unwrap_or(0) as f64;
		let inverse_document_frequency = (1.0
			+ (document_count as f64 - document_frequency + 0.5) / (document_frequency + 0.5))
			.ln();
		let frequency = *frequency as f64;
		let normalization =
			frequency + k1 * (1.0 - b + b * document_length as f64 / average_length);
		score + inverse_document_frequency * frequency * (k1 + 1.0) / normalization
	})
}

fn merge_spans(spans: &mut Vec<(u32, u32)>) {
	let mut write = 0_usize;
	for read in 0..spans.len() {
		if write > 0 && spans[read].0 <= spans[write - 1].1 {
			spans[write - 1].1 = spans[write - 1].1.max(spans[read].1);
		} else {
			spans[write] = spans[read];
			write += 1;
		}
	}
	spans.truncate(write);
}

#[derive(Debug)]
struct HeapCandidate {
	distance: f64,
	stable: Vec<Value>,
	ordinal: usize,
}

impl PartialEq for HeapCandidate {
	fn eq(&self, other: &Self) -> bool {
		self.distance.to_bits() == other.distance.to_bits()
			&& self.stable == other.stable
			&& self.ordinal == other.ordinal
	}
}

impl Eq for HeapCandidate {}

impl PartialOrd for HeapCandidate {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for HeapCandidate {
	fn cmp(&self, other: &Self) -> Ordering {
		self.distance
			.total_cmp(&other.distance)
			.then_with(|| self.stable.cmp(&other.stable))
			.then_with(|| self.ordinal.cmp(&other.ordinal))
	}
}

#[allow(clippy::too_many_arguments, reason = "exact KNN uses the complete SELECT context")]
fn prepare_knn<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	rows: RowSet,
	filter: &TypedExpr,
	expression: &TypedExpr,
	prepared: &PreparedKnn,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<RowSet> {
	let TypedExprKind::Knn {
		left,
		..
	} = &expression.kind
	else {
		unreachable!("KNN collection retained a non-KNN expression")
	};
	let mut heap = BinaryHeap::with_capacity(prepared.k.saturating_add(1));
	let mut ordinal = 0_usize;
	rows.visit(|row| {
		control.check()?;
		let current = ordinal;
		ordinal = ordinal
			.checked_add(1)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("KNN row ordinal overflow"))?;
		let mut residual = row.clone();
		residual.knn.push(KnnMatch {
			predicate: prepared.predicate,
			distance_bits: 0.0_f64.to_bits(),
		});
		if !eval_select_expr(read, snapshot, filter, &residual, None, slots, source, control)?
			.is_truthy()
		{
			return Ok(());
		}
		let value = eval_select_expr(read, snapshot, left, &row, None, slots, source, control)?;
		let Value::Vector(vector) = value else {
			if matches!(value, Value::None | Value::Null) {
				return Ok(());
			}
			return Err(FerrosGrynnError::invalid_argument(
				"KNN field produced a non-vector value",
			));
		};
		let distance = prepared.distance.evaluate(&vector, &prepared.query, control, source)?;
		heap.push(HeapCandidate {
			distance,
			stable: row.stable.clone(),
			ordinal: current,
		});
		if heap.len() > prepared.k {
			heap.pop();
		}
		Ok(())
	})?;
	let selected = heap
		.into_iter()
		.map(|candidate| (candidate.ordinal, candidate.distance))
		.collect::<BTreeMap<_, _>>();
	let mut selected_rows = Vec::with_capacity(selected.len());
	let mut ordinal = 0_usize;
	rows.consume(|mut row| {
		if let Some(distance) = selected.get(&ordinal).copied() {
			row.knn.push(KnnMatch {
				predicate: prepared.predicate,
				distance_bits: distance.to_bits(),
			});
			selected_rows.push((distance, row.stable.clone(), ordinal, row));
		}
		ordinal = ordinal
			.checked_add(1)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("KNN row ordinal overflow"))?;
		Ok(())
	})?;
	selected_rows.sort_by(|left, right| {
		left.0
			.total_cmp(&right.0)
			.then_with(|| left.1.cmp(&right.1))
			.then_with(|| left.2.cmp(&right.2))
	});
	let mut output = RowCollector::new(read, control, false);
	for (_, _, _, row) in selected_rows {
		output.push(read, snapshot, row)?;
	}
	output.finish()
}

fn prepare_knn_input(
	expression: &TypedExpr,
	access: &LogicalSelectAccess,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<PreparedKnn> {
	let TypedExprKind::Knn {
		k,
		ef,
		distance,
		right,
		definitions,
		..
	} = &expression.kind
	else {
		unreachable!("KNN collection retained a non-KNN expression")
	};
	let definition = choose_vector_definition(definitions, access, ef.is_some())?;
	let k = positive_usize(eval(k, None, slots, source, None, Some(control))?, "KNN K")?;
	let ef = if let Some(ef) = ef {
		let ef = positive_usize(eval(ef, None, slots, source, None, Some(control))?, "KNN EF")?;
		if ef < k {
			return Err(FerrosGrynnError::invalid_argument("KNN EF must be at least K"));
		}
		Some(ef)
	} else {
		None
	};
	let Value::Vector(query) = eval(right, None, slots, source, None, Some(control))? else {
		return Err(FerrosGrynnError::invalid_argument("KNN query must evaluate to a vector"));
	};
	Ok(PreparedKnn {
		predicate: expression.span.offset,
		k,
		ef,
		query,
		distance: runtime_distance(distance, slots, source, control)?,
		definition,
	})
}

fn choose_vector_definition(
	definitions: &[LogicalVectorDefinition],
	access: &LogicalSelectAccess,
	requires_ann: bool,
) -> FerrosGrynnResult<Option<LogicalVectorDefinition>> {
	let forced = match access {
		LogicalSelectAccess::Indexes(hints) => definitions
			.iter()
			.filter(|definition| {
				hints.iter().any(|hint| {
					hint.index.as_ref() == definition.name.as_str()
						&& hint.source.as_deref().is_none_or(|source| {
							source.eq_ignore_ascii_case(definition.source.as_ref())
						})
				})
			})
			.collect::<Vec<_>>(),
		LogicalSelectAccess::Automatic | LogicalSelectAccess::NoIndex => Vec::new(),
	};
	if forced.len() > 1 {
		return Err(FerrosGrynnError::invalid_argument(
			"KNN WITH INDEX candidates select more than one compatible ANN definition",
		));
	}
	if let Some(definition) = forced.first()
		&& definition.state != crate::datastore::ClassIndexState::Ready
	{
		return Err(FerrosGrynnError::invalid_argument(format!(
			"forced ANN index `{}` is not Ready",
			definition.name
		)));
	}
	if requires_ann {
		let ready = definitions
			.iter()
			.filter(|definition| definition.state == crate::datastore::ClassIndexState::Ready)
			.collect::<Vec<_>>();
		if ready.len() != 1 {
			return Err(FerrosGrynnError::invalid_argument(
				"KNN EF requires exactly one compatible Ready ANN index",
			));
		}
		return Ok(Some(ready[0].clone()));
	}
	if let Some(definition) = forced.first() {
		return Ok(Some((**definition).clone()));
	}
	Ok(definitions
		.iter()
		.filter(|definition| definition.state == crate::datastore::ClassIndexState::Ready)
		.min_by_key(|definition| definition.index)
		.cloned())
}

enum RuntimeDistance {
	Builtin(crate::datastore::VectorDistance),
	Named(NamedFunctionKey),
	Closure(crate::values::Closure),
}

impl RuntimeDistance {
	fn evaluate(
		&self,
		left: &crate::values::Vector,
		right: &crate::values::Vector,
		control: &ExecutionControl,
		source: &str,
	) -> FerrosGrynnResult<f64> {
		let distance = match self {
			Self::Builtin(distance) => {
				crate::datastore::exact_vector_distance(left, right, *distance)?
			}
			Self::Named(function) => {
				let plan = control.resolve_named_function(*function)?;
				let _frame = control.enter_named_function(*function)?;
				let value = invoke_logical_closure(
					control,
					&plan,
					&[],
					&[Value::Vector(left.clone()), Value::Vector(right.clone())],
					source,
				)?;
				float_distance(value)?
			}
			Self::Closure(closure) => {
				let value = invoke_expression_closure(
					control,
					closure,
					&[Value::Vector(left.clone()), Value::Vector(right.clone())],
					source,
				)?;
				float_distance(value)?
			}
		};
		if !distance.is_finite() {
			return Err(FerrosGrynnError::invalid_argument("vector distance must be finite"));
		}
		if matches!(self, Self::Named(_) | Self::Closure(_)) && distance < 0.0 {
			return Err(FerrosGrynnError::invalid_argument(
				"custom vector distance must be finite and non-negative",
			));
		}
		Ok(distance)
	}
}

fn runtime_distance(
	distance: &LogicalKnnDistance,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<RuntimeDistance> {
	match distance {
		LogicalKnnDistance::Builtin(distance) => Ok(RuntimeDistance::Builtin(*distance)),
		LogicalKnnDistance::Minkowski(exponent) => {
			let value = eval(exponent, None, slots, source, None, Some(control))?;
			let exponent =
				value.as_number().and_then(|number| f64::try_from(number).ok()).ok_or_else(
					|| FerrosGrynnError::invalid_argument("MINKOWSKI exponent must be numeric"),
				)?;
			let distance = crate::datastore::VectorDistance::Minkowski(exponent);
			distance.validate()?;
			Ok(RuntimeDistance::Builtin(distance))
		}
		LogicalKnnDistance::Named(function) => Ok(RuntimeDistance::Named(*function)),
		LogicalKnnDistance::Callable(callable) => {
			let Value::Closure(closure) = eval(callable, None, slots, source, None, Some(control))?
			else {
				return Err(FerrosGrynnError::invalid_argument(
					"custom KNN distance must evaluate to a closure",
				));
			};
			let entry = control.resolve_closure(&closure)?;
			if entry.plan.effect != ClosureEffect::Pure {
				return Err(FerrosGrynnError::invalid_argument(
					"custom KNN distance closure must be PURE",
				));
			}
			Ok(RuntimeDistance::Closure(*closure))
		}
	}
}

fn float_distance(value: Value) -> FerrosGrynnResult<f64> {
	match value {
		Value::Number(Number::Float(value)) => Ok(value),
		_ => Err(FerrosGrynnError::invalid_argument("custom vector distance must return float")),
	}
}

fn positive_usize(value: Value, name: &str) -> FerrosGrynnResult<usize> {
	let Value::Number(number) = value else {
		return Err(FerrosGrynnError::invalid_argument(format!(
			"{name} must be a positive integer"
		)));
	};
	let value = usize::try_from(number).map_err(|_| {
		FerrosGrynnError::invalid_argument(format!("{name} must be a positive integer"))
	})?;
	if value == 0 {
		return Err(FerrosGrynnError::invalid_argument(format!(
			"{name} must be a positive integer"
		)));
	}
	Ok(value)
}

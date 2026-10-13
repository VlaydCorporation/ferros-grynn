//! Shared span-independent canonical hashing of analyzed expressions.

use std::hash::{Hash, Hasher};

use super::{CanonicalHasher, PLAN_ENCODING_VERSION};
use crate::gql::ir::{
	LogicalGraphReference,
	LogicalKnnDistance,
	LogicalSelect,
	LogicalSelectAccess,
	LogicalSelectItem,
	LogicalSelectSource,
	LogicalSelectSourceKind,
	LogicalTraverseSource,
	LogicalWildcardField,
	ScanTarget,
	TypedExpr,
	TypedExprKind,
};

/// Existing ordering identity retained across the module move.
const COLLATION_FINGERPRINT: &[u8] = b"icu4x-2.2.1-root-primary-tertiary-numeric-v1";

/// Hashes exact binding eligibility independently of source spans and aliases.
pub(super) fn hash_match_binding(binding: &crate::gql::ir::MatchBinding, state: &mut impl Hasher) {
	std::mem::discriminant(&binding.domain).hash(state);
	std::mem::discriminant(&binding.selector).hash(state);
	match &binding.selector {
		crate::gql::ir::LogicalMatchSelector::Any => {}
		crate::gql::ir::LogicalMatchSelector::Class(class) => class.hash(state),
		crate::gql::ir::LogicalMatchSelector::Record(record) => {
			record.class.hash(state);
			hash_expr_inner(&record.key, state, None);
		}
	}
	binding.label.hash(state);
	binding.meta.hash(state);
	for (field, expression) in &binding.properties {
		field.hash(state);
		hash_expr_inner(expression, state, None);
	}
	hash_optional_expr_inner(binding.predicate.as_ref(), state, None);
}

/// Returns normalized SELECT relation and ordering identities.
pub(in crate::gql) fn fingerprints(plan: &LogicalSelect) -> (u64, u64) {
	fingerprints_inner(plan, None)
}

/// Exact graph cursors identify lexical parameters independently of frame
/// ordinals.
pub(in crate::gql) fn cursor_fingerprints(plan: &LogicalSelect, text: &str) -> (u64, u64) {
	fingerprints_inner(plan, Some(text))
}

/// Hashes the logical recipe, including graph terminal selectors, for cursor
/// validation.
fn fingerprints_inner(plan: &LogicalSelect, text: Option<&str>) -> (u64, u64) {
	let mut query = CanonicalHasher::versioned(PLAN_ENCODING_VERSION, b"select-query");
	hash_select_inner(plan, &mut query, text);
	let mut order = CanonicalHasher::versioned(PLAN_ENCODING_VERSION, b"select-order");
	for item in &plan.order {
		item.random.hash(&mut order);
		item.collate.hash(&mut order);
		item.numeric.hash(&mut order);
		if item.collate || item.numeric {
			COLLATION_FINGERPRINT.hash(&mut order);
		}
		hash_expr_inner(&item.expression, &mut order, text);
		std::mem::discriminant(&item.direction).hash(&mut order);
		std::mem::discriminant(&item.nulls).hash(&mut order);
	}
	(query.finish(), order.finish())
}

/// Encodes analyzed expression semantics without text spans.
pub(super) fn hash_expr(expression: &TypedExpr, state: &mut impl Hasher) {
	hash_expr_inner(expression, state, None);
}

/// Encodes relation semantics while excluding cursor page state.
fn hash_select_inner(plan: &LogicalSelect, state: &mut impl Hasher, text: Option<&str>) {
	if !plan.union_inputs.is_empty() {
		match plan.union_purpose {
			crate::syn::SelectUnionPurpose::IndependentOutputs => {
				"independent SELECT union".hash(state);
			}
			crate::syn::SelectUnionPurpose::JoinedTypingVariants => {
				"JOIN typing family".hash(state);
			}
		}
		plan.union_inputs.len().hash(state);
		for input in &plan.union_inputs {
			hash_select_inner(input, state, text);
		}
	}
	hash_source_inner(&plan.source, state, text);
	for join in &plan.joins {
		std::mem::discriminant(&join.kind).hash(state);
		hash_source_inner(&join.source, state, text);
		hash_optional_expr_inner(join.predicate.as_ref(), state, text);
	}
	match &plan.access {
		LogicalSelectAccess::Automatic => 0_u8.hash(state),
		LogicalSelectAccess::NoIndex => 1_u8.hash(state),
		LogicalSelectAccess::Indexes(hints) => {
			2_u8.hash(state);
			for hint in hints {
				hint.source.hash(state);
				hint.index.hash(state);
			}
		}
	}
	hash_optional_expr_inner(plan.filter.as_ref(), state, text);
	for expression in &plan.split {
		hash_expr_inner(expression, state, text);
	}
	match &plan.group {
		None => 0_u8.hash(state),
		Some(keys) => {
			1_u8.hash(state);
			for key in keys {
				hash_expr_inner(key, state, text);
			}
		}
	}
	for item in &plan.projection {
		match item {
			LogicalSelectItem::Wildcard(fields) => {
				0_u8.hash(state);
				for field in fields {
					match field {
						LogicalWildcardField::Column {
							name,
							index,
						} => {
							0_u8.hash(state);
							name.hash(state);
							index.hash(state);
						}
						LogicalWildcardField::Properties {
							index,
						} => {
							1_u8.hash(state);
							index.hash(state);
						}
					}
				}
			}
			LogicalSelectItem::WildcardVariants {
				selector,
				choices,
			} => {
				2_u8.hash(state);
				hash_expr_inner(selector, state, text);
				choices.len().hash(state);
				for fields in choices {
					fields.len().hash(state);
					for field in fields {
						match field {
							LogicalWildcardField::Column {
								name,
								index,
							} => {
								0_u8.hash(state);
								name.hash(state);
								index.hash(state);
							}
							LogicalWildcardField::Properties {
								index,
							} => {
								1_u8.hash(state);
								index.hash(state);
							}
						}
					}
				}
			}
			LogicalSelectItem::Expression {
				name,
				expression,
			} => {
				1_u8.hash(state);
				name.hash(state);
				hash_expr_inner(expression, state, text);
			}
		}
	}
	plan.omit.hash(state);
	plan.fetch.hash(state);
	if !plan.projection_paths.is_empty() {
		b"projection-paths-v1".hash(state);
		plan.projection_paths.hash(state);
	}
	for item in &plan.order {
		item.random.hash(state);
		item.collate.hash(state);
		item.numeric.hash(state);
		if item.collate || item.numeric {
			COLLATION_FINGERPRINT.hash(state);
		}
		hash_expr_inner(&item.expression, state, text);
		std::mem::discriminant(&item.direction).hash(state);
		std::mem::discriminant(&item.nulls).hash(state);
	}
	// LIMIT and START are page state, not part of the normalized relation and
	// ordering identity validated by the cursor runtime.
	plan.value.hash(state);
	plan.only.hash(state);
	plan.tempfiles.hash(state);
	plan.outer_columns.hash(state);
	if let Some(graph) = &plan.return_graph {
		b"select-return-graph-v1".hash(state);
		graph.contract.family.hash(state);
		graph.contract.validators.bits().hash(state);
		for expression in graph.expressions() {
			hash_expr_inner(expression, state, text);
		}
		graph.root.is_some().hash(state);
	}
}

/// Encodes a declared atom, graph-catalog or derived source.
fn hash_source_inner(source: &LogicalSelectSource, state: &mut impl Hasher, text: Option<&str>) {
	source.alias.hash(state);
	match &source.kind {
		LogicalSelectSourceKind::Empty {
			graph,
		} => {
			3_u8.hash(state);
			graph.is_some().hash(state);
			if let Some(graph) = graph {
				hash_graph_reference_inner(graph, state, text);
			}
		}
		LogicalSelectSourceKind::Atoms {
			target,
			graph,
		} => {
			0_u8.hash(state);
			match target {
				ScanTarget::Class(class) => {
					0_u8.hash(state);
					class.hash(state);
				}
				ScanTarget::Record(record) => {
					1_u8.hash(state);
					record.hash(state);
				}
			}
			if let Some(graph) = graph {
				hash_graph_reference_inner(graph, state, text);
			}
		}
		LogicalSelectSourceKind::Graphs {
			references,
		} => {
			1_u8.hash(state);
			if let Some(references) = references {
				for reference in references {
					hash_graph_reference_inner(reference, state, text);
				}
			}
		}
		LogicalSelectSourceKind::AtomUnion {
			targets,
			graph,
			tag_column,
			typing,
		} => {
			4_u8.hash(state);
			typing.hash(state);
			targets.len().hash(state);
			for target in targets {
				target.class().hash(state);
			}
			tag_column.hash(state);
			graph.is_some().hash(state);
			if let Some(graph) = graph {
				hash_graph_reference_inner(graph, state, text);
			}
		}
		LogicalSelectSourceKind::Derived(select) => {
			2_u8.hash(state);
			hash_select_inner(select, state, text);
		}
	}
}

/// Distinguishes static graph identity from an analyzed runtime expression.
fn hash_graph_reference_inner(
	reference: &LogicalGraphReference,
	state: &mut impl Hasher,
	text: Option<&str>,
) {
	match reference {
		LogicalGraphReference::Static(reference) => {
			0_u8.hash(state);
			reference.hash(state);
		}
		LogicalGraphReference::Expression(expression) => {
			1_u8.hash(state);
			hash_expr_inner(expression, state, text);
		}
	}
}

/// Frames presence separately from expression bytes.
fn hash_optional_expr_inner(
	expression: Option<&TypedExpr>,
	state: &mut impl Hasher,
	text: Option<&str>,
) {
	match expression {
		None => 0_u8.hash(state),
		Some(expression) => {
			1_u8.hash(state);
			hash_expr_inner(expression, state, text);
		}
	}
}

/// Encodes analyzed expression semantics without text spans.
fn hash_expr_inner(expression: &TypedExpr, state: &mut impl Hasher, text: Option<&str>) {
	std::mem::discriminant(&expression.kind).hash(state);
	match &expression.kind {
		TypedExprKind::Constant(value) => value.hash(state),
		TypedExprKind::Slot(slot) | TypedExprKind::LiveSlot(slot) => {
			if let Some(name) = text.and_then(|text| {
				expression
					.span
					.end()
					.and_then(|end| text.get(expression.span.offset as usize..end as usize))
			}) {
				name.hash(state);
			} else {
				slot.hash(state);
			}
		}
		TypedExprKind::Column(column) => column.hash(state),
		TypedExprKind::Array(values) | TypedExprKind::Set(values) => {
			for value in values {
				hash_expr_inner(value, state, text);
			}
		}
		TypedExprKind::Object(fields) => {
			for (name, value) in fields {
				name.hash(state);
				hash_expr_inner(value, state, text);
			}
		}
		TypedExprKind::Range {
			start,
			end,
			inclusive_end,
		} => {
			hash_optional_expr_inner(start.as_deref(), state, text);
			hash_optional_expr_inner(end.as_deref(), state, text);
			inclusive_end.hash(state);
		}
		TypedExprKind::Unary {
			op,
			expr,
		} => {
			std::mem::discriminant(op).hash(state);
			hash_expr_inner(expr, state, text);
		}
		TypedExprKind::Binary {
			left,
			op,
			right,
		} => {
			hash_expr_inner(left, state, text);
			std::mem::discriminant(op).hash(state);
			hash_expr_inner(right, state, text);
		}
		TypedExprKind::Fulltext {
			left,
			op,
			right,
			definitions,
		} => {
			hash_expr_inner(left, state, text);
			std::mem::discriminant(op).hash(state);
			if let crate::syn::FulltextOperator::Slot(slot) = op {
				slot.hash(state);
			}
			hash_expr_inner(right, state, text);
			for definition in definitions {
				definition.index.hash(state);
				definition.scope.hash(state);
				definition.analyzer.id().hash(state);
				definition.analyzer.version().hash(state);
				definition.analyzer_function.hash(state);
				definition.k1.to_bits().hash(state);
				definition.b.to_bits().hash(state);
				definition.highlights.hash(state);
			}
		}
		TypedExprKind::Knn {
			left,
			k,
			ef,
			distance,
			right,
			definitions,
			..
		} => {
			hash_expr_inner(left, state, text);
			hash_expr_inner(k, state, text);
			hash_optional_expr_inner(ef.as_deref(), state, text);
			match distance {
				LogicalKnnDistance::Builtin(distance) => {
					0_u8.hash(state);
					distance.hash(state);
				}
				LogicalKnnDistance::Minkowski(exponent) => {
					1_u8.hash(state);
					hash_expr_inner(exponent, state, text);
				}
				LogicalKnnDistance::Named(function) => {
					2_u8.hash(state);
					function.hash(state);
				}
				LogicalKnnDistance::Callable(callable) => {
					3_u8.hash(state);
					hash_expr_inner(callable, state, text);
				}
			}
			hash_expr_inner(right, state, text);
			for definition in definitions {
				definition.index.hash(state);
				definition.scope.hash(state);
				std::mem::discriminant(&definition.kind).hash(state);
				match definition.options {
					crate::datastore::ClassIndexOptions::Scalar => 0_u8.hash(state),
					crate::datastore::ClassIndexOptions::Geometry(options) => {
						4_u8.hash(state);
						options.hash(state);
					}
					crate::datastore::ClassIndexOptions::Fulltext(options) => {
						1_u8.hash(state);
						options.hash(state);
					}
					crate::datastore::ClassIndexOptions::Hnsw(options) => {
						2_u8.hash(state);
						options.hash(state);
					}
					crate::datastore::ClassIndexOptions::DiskAnn(options) => {
						3_u8.hash(state);
						options.hash(state);
					}
				}
			}
		}
		TypedExprKind::Conditional {
			condition,
			then_value,
			else_value,
		} => {
			hash_expr_inner(condition, state, text);
			hash_expr_inner(then_value, state, text);
			hash_expr_inner(else_value, state, text);
		}
		TypedExprKind::Case {
			operand,
			branches,
			else_value,
		} => {
			hash_optional_expr_inner(operand.as_deref(), state, text);
			for (condition, value) in branches {
				hash_expr_inner(condition, state, text);
				hash_expr_inner(value, state, text);
			}
			hash_optional_expr_inner(else_value.as_deref(), state, text);
		}
		TypedExprKind::Field {
			base,
			field,
			null_safe,
		} => {
			hash_expr_inner(base, state, text);
			field.hash(state);
			null_safe.hash(state);
		}
		TypedExprKind::CurrentField(field) => field.hash(state),
		TypedExprKind::Destructure {
			base,
			fields,
		} => {
			hash_expr_inner(base, state, text);
			fields.hash(state);
		}
		TypedExprKind::Index {
			base,
			index,
		} => {
			hash_expr_inner(base, state, text);
			hash_expr_inner(index, state, text);
		}
		TypedExprKind::Filter {
			base,
			predicate,
		} => {
			hash_expr_inner(base, state, text);
			hash_expr_inner(predicate, state, text);
		}
		TypedExprKind::Builtin {
			function,
			arguments,
		} => {
			std::mem::discriminant(function).hash(state);
			for argument in arguments {
				hash_expr_inner(argument, state, text);
			}
		}
		TypedExprKind::Closure(closure) => {
			if let Some(body) = text.and_then(|text| {
				expression
					.span
					.end()
					.and_then(|end| text.get(expression.span.offset as usize..end as usize))
			}) {
				// Cursor closures retain their complete source, including block
				// control flow. shortcut: equivalent
				// reformatted closures require a new cursor; normalize lexical
				// tokens when that use case is supported.
				body.hash(state);
				closure.result.hash(state);
				std::mem::discriminant(&closure.effect).hash(state);
				closure.named_function.hash(state);
				return;
			}
			closure.parameters.hash(state);
			closure.captures.hash(state);
			closure.result.hash(state);
			std::mem::discriminant(&closure.effect).hash(state);
			closure.slot_count.hash(state);
			closure.named_function.hash(state);
			if let crate::gql::ir::LogicalClosureBody::Expression(body) = &closure.body {
				hash_expr_inner(body, state, text);
			}
		}
		TypedExprKind::Invoke {
			callee,
			arguments,
		} => {
			hash_expr_inner(callee, state, text);
			for argument in arguments {
				hash_expr_inner(argument, state, text);
			}
		}
		TypedExprKind::NamedInvoke {
			function,
			arguments,
		} => {
			function.hash(state);
			for argument in arguments {
				hash_expr_inner(argument, state, text);
			}
		}
		TypedExprKind::Traversal(traverse) => {
			std::mem::discriminant(&traverse.source).hash(state);
			if let LogicalTraverseSource::Expression(source) = &traverse.source {
				hash_expr_inner(source, state, text);
			}
			for step in &traverse.steps {
				step.kind.hash(state);
				step.named_function.hash(state);
				step.algorithm_input.hash(state);
				step.output_type.hash(state);
				for argument in &step.arguments {
					hash_expr_inner(argument, state, text);
				}
			}
		}
		TypedExprKind::Aggregate {
			function,
			argument,
			distinct,
		} => {
			std::mem::discriminant(function).hash(state);
			hash_optional_expr_inner(argument.as_deref(), state, text);
			distinct.hash(state);
		}
		TypedExprKind::Subquery {
			select,
			collection,
		} => {
			hash_select_inner(select, state, text);
			collection.hash(state);
		}
		TypedExprKind::Cast {
			target,
			expr,
		} => {
			target.hash(state);
			hash_expr_inner(expr, state, text);
		}
	}
}

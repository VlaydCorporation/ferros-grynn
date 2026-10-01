use std::hash::{Hash, Hasher};

use super::super::*;
use crate::gql::{
	ir::LogicalKnnDistance,
	optimizer::{CanonicalHasher, PLAN_ENCODING_VERSION},
};

const COLLATION_FINGERPRINT: &[u8] = b"icu4x-2.2.1-root-primary-tertiary-numeric-v1";

pub(in crate::gql::execution) fn fingerprints(plan: &LogicalSelect) -> (u64, u64) {
	let mut query = CanonicalHasher::versioned(PLAN_ENCODING_VERSION, b"select-query");
	hash_select(plan, &mut query);
	let mut order = CanonicalHasher::versioned(PLAN_ENCODING_VERSION, b"select-order");
	for item in &plan.order {
		item.random.hash(&mut order);
		item.collate.hash(&mut order);
		item.numeric.hash(&mut order);
		if item.collate || item.numeric {
			COLLATION_FINGERPRINT.hash(&mut order);
		}
		hash_expr(&item.expression, &mut order);
		std::mem::discriminant(&item.direction).hash(&mut order);
		std::mem::discriminant(&item.nulls).hash(&mut order);
	}
	(query.finish(), order.finish())
}

fn hash_select(plan: &LogicalSelect, state: &mut impl Hasher) {
	hash_source(&plan.source, state);
	for join in &plan.joins {
		std::mem::discriminant(&join.kind).hash(state);
		hash_source(&join.source, state);
		hash_optional_expr(join.predicate.as_ref(), state);
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
	hash_optional_expr(plan.filter.as_ref(), state);
	for expression in &plan.split {
		hash_expr(expression, state);
	}
	match &plan.group {
		None => 0_u8.hash(state),
		Some(keys) => {
			1_u8.hash(state);
			for key in keys {
				hash_expr(key, state);
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
			LogicalSelectItem::Expression {
				name,
				expression,
			} => {
				1_u8.hash(state);
				name.hash(state);
				hash_expr(expression, state);
			}
		}
	}
	plan.omit.hash(state);
	plan.fetch.hash(state);
	for item in &plan.order {
		item.random.hash(state);
		item.collate.hash(state);
		item.numeric.hash(state);
		if item.collate || item.numeric {
			COLLATION_FINGERPRINT.hash(state);
		}
		hash_expr(&item.expression, state);
		std::mem::discriminant(&item.direction).hash(state);
		std::mem::discriminant(&item.nulls).hash(state);
	}
	// LIMIT and START are page state, not part of the normalized relation and
	// ordering identity validated by the cursor runtime.
	plan.value.hash(state);
	plan.only.hash(state);
	plan.tempfiles.hash(state);
	plan.outer_columns.hash(state);
}

fn hash_source(source: &LogicalSelectSource, state: &mut impl Hasher) {
	source.alias.hash(state);
	match &source.kind {
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
				hash_graph_reference(graph, state);
			}
		}
		LogicalSelectSourceKind::Graphs {
			references,
		} => {
			1_u8.hash(state);
			if let Some(references) = references {
				for reference in references {
					hash_graph_reference(reference, state);
				}
			}
		}
		LogicalSelectSourceKind::Derived(select) => {
			2_u8.hash(state);
			hash_select(select, state);
		}
	}
}

fn hash_graph_reference(reference: &LogicalGraphReference, state: &mut impl Hasher) {
	match reference {
		LogicalGraphReference::Static(reference) => {
			0_u8.hash(state);
			reference.hash(state);
		}
		LogicalGraphReference::Expression(expression) => {
			1_u8.hash(state);
			hash_expr(expression, state);
		}
	}
}

fn hash_optional_expr(expression: Option<&TypedExpr>, state: &mut impl Hasher) {
	match expression {
		None => 0_u8.hash(state),
		Some(expression) => {
			1_u8.hash(state);
			hash_expr(expression, state);
		}
	}
}

fn hash_expr(expression: &TypedExpr, state: &mut impl Hasher) {
	std::mem::discriminant(&expression.kind).hash(state);
	match &expression.kind {
		TypedExprKind::Constant(value) => value.hash(state),
		TypedExprKind::Slot(slot) => slot.hash(state),
		TypedExprKind::Column(column) => column.hash(state),
		TypedExprKind::Array(values) | TypedExprKind::Set(values) => {
			for value in values {
				hash_expr(value, state);
			}
		}
		TypedExprKind::Object(fields) => {
			for (name, value) in fields {
				name.hash(state);
				hash_expr(value, state);
			}
		}
		TypedExprKind::Range {
			start,
			end,
			inclusive_end,
		} => {
			hash_optional_expr(start.as_deref(), state);
			hash_optional_expr(end.as_deref(), state);
			inclusive_end.hash(state);
		}
		TypedExprKind::Unary {
			op,
			expr,
		} => {
			std::mem::discriminant(op).hash(state);
			hash_expr(expr, state);
		}
		TypedExprKind::Binary {
			left,
			op,
			right,
		} => {
			hash_expr(left, state);
			std::mem::discriminant(op).hash(state);
			hash_expr(right, state);
		}
		TypedExprKind::Fulltext {
			left,
			op,
			right,
			definitions,
		} => {
			hash_expr(left, state);
			std::mem::discriminant(op).hash(state);
			if let crate::syn::FulltextOperator::Slot(slot) = op {
				slot.hash(state);
			}
			hash_expr(right, state);
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
			hash_expr(left, state);
			hash_expr(k, state);
			hash_optional_expr(ef.as_deref(), state);
			match distance {
				LogicalKnnDistance::Builtin(distance) => {
					0_u8.hash(state);
					distance.hash(state);
				}
				LogicalKnnDistance::Minkowski(exponent) => {
					1_u8.hash(state);
					hash_expr(exponent, state);
				}
				LogicalKnnDistance::Named(function) => {
					2_u8.hash(state);
					function.hash(state);
				}
				LogicalKnnDistance::Callable(callable) => {
					3_u8.hash(state);
					hash_expr(callable, state);
				}
			}
			hash_expr(right, state);
			for definition in definitions {
				definition.index.hash(state);
				definition.scope.hash(state);
				std::mem::discriminant(&definition.kind).hash(state);
				match definition.options {
					crate::datastore::ClassIndexOptions::Scalar => 0_u8.hash(state),
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
			hash_expr(condition, state);
			hash_expr(then_value, state);
			hash_expr(else_value, state);
		}
		TypedExprKind::Case {
			operand,
			branches,
			else_value,
		} => {
			hash_optional_expr(operand.as_deref(), state);
			for (condition, value) in branches {
				hash_expr(condition, state);
				hash_expr(value, state);
			}
			hash_optional_expr(else_value.as_deref(), state);
		}
		TypedExprKind::Field {
			base,
			field,
			null_safe,
		} => {
			hash_expr(base, state);
			field.hash(state);
			null_safe.hash(state);
		}
		TypedExprKind::CurrentField(field) => field.hash(state),
		TypedExprKind::Destructure {
			base,
			fields,
		} => {
			hash_expr(base, state);
			fields.hash(state);
		}
		TypedExprKind::Index {
			base,
			index,
		} => {
			hash_expr(base, state);
			hash_expr(index, state);
		}
		TypedExprKind::Filter {
			base,
			predicate,
		} => {
			hash_expr(base, state);
			hash_expr(predicate, state);
		}
		TypedExprKind::Builtin {
			function,
			arguments,
		} => {
			std::mem::discriminant(function).hash(state);
			for argument in arguments {
				hash_expr(argument, state);
			}
		}
		TypedExprKind::Closure(closure) => {
			closure.parameters.hash(state);
			closure.captures.hash(state);
			closure.result.hash(state);
			std::mem::discriminant(&closure.effect).hash(state);
			closure.slot_count.hash(state);
			closure.named_function.hash(state);
			if let crate::gql::ir::LogicalClosureBody::Expression(body) = &closure.body {
				hash_expr(body, state);
			}
		}
		TypedExprKind::Invoke {
			callee,
			arguments,
		} => {
			hash_expr(callee, state);
			for argument in arguments {
				hash_expr(argument, state);
			}
		}
		TypedExprKind::NamedInvoke {
			function,
			arguments,
		} => {
			function.hash(state);
			for argument in arguments {
				hash_expr(argument, state);
			}
		}
		TypedExprKind::Traversal(traverse) => {
			std::mem::discriminant(&traverse.source).hash(state);
			if let LogicalTraverseSource::Expression(source) = &traverse.source {
				hash_expr(source, state);
			}
			for step in &traverse.steps {
				step.kind.hash(state);
				step.named_function.hash(state);
				step.algorithm_input.hash(state);
				step.output_type.hash(state);
				for argument in &step.arguments {
					hash_expr(argument, state);
				}
			}
		}
		TypedExprKind::Aggregate {
			function,
			argument,
			distinct,
		} => {
			std::mem::discriminant(function).hash(state);
			hash_optional_expr(argument.as_deref(), state);
			distinct.hash(state);
		}
		TypedExprKind::Subquery {
			select,
			collection,
		} => {
			hash_select(select, state);
			collection.hash(state);
		}
		TypedExprKind::Cast {
			target,
			expr,
		} => {
			target.hash(state);
			hash_expr(expr, state);
		}
	}
}

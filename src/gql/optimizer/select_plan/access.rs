//! Source access paths, scalar-index constraints, and ordering compatibility.

use std::collections::{BTreeMap, BTreeSet};

use super::{
	super::{
		Cardinality,
		CostWeights,
		IndexAccessKind,
		IndexDirection,
		IndexJoinProbe,
		PhysicalSource,
		PlanCost,
		PlanWarning,
		PlannerIndex,
		PlannerSnapshot,
		SourceOperator,
		estimate_predicate,
		lookup_cost,
		range_lookup_cost,
		scan_cost,
	},
	plan_select,
};
use crate::{
	datastore::{ClassIndexKind, ClassIndexState, IndexScope},
	gql::ir::{
		Aggregate,
		LogicalGraphReference,
		LogicalSelect,
		LogicalSelectAccess,
		LogicalSelectItem,
		LogicalSelectSource,
		LogicalSelectSourceKind,
		LogicalWildcardField,
		ScanTarget,
		TypedExpr,
		TypedExprKind,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	syn::BinaryOperator,
};

pub(super) fn resolve_hints(
	logical: &LogicalSelect,
	snapshot: &PlannerSnapshot,
	sources: &[&LogicalSelectSource],
) -> FerrosGrynnResult<BTreeMap<usize, BTreeSet<Box<str>>>> {
	let LogicalSelectAccess::Indexes(requested) = &logical.access else {
		return Ok(BTreeMap::new());
	};
	let mut hints = BTreeMap::<usize, BTreeSet<Box<str>>>::new();
	for hint in requested {
		let source = if let Some(alias) = &hint.source {
			let matches = sources
				.iter()
				.enumerate()
				.filter_map(|(index, source)| {
					source.alias.eq_ignore_ascii_case(alias).then_some(index)
				})
				.collect::<Vec<_>>();
			match matches.as_slice() {
				[index] => *index,
				[] => {
					return Err(index_hint_error(
						hint.index.as_ref(),
						alias,
						"the source alias does not exist",
					));
				}
				_ => unreachable!("analyzer rejects duplicate aliases"),
			}
		} else {
			let matches = sources
				.iter()
				.enumerate()
				.filter_map(|(index, source)| source_class(source).map(|class| (index, class)))
				.filter_map(|(index, class)| {
					snapshot
						.class(class)
						.is_some_and(|class| {
							class
								.indexes
								.iter()
								.any(|index| index.name.as_str() == hint.index.as_ref())
						})
						.then_some(index)
				})
				.collect::<Vec<_>>();
			match matches.as_slice() {
				[index] => *index,
				[] => {
					return Err(index_hint_error(
						hint.index.as_ref(),
						"SELECT",
						"the index does not exist on a source",
					));
				}
				_ => {
					return Err(index_hint_error(
						hint.index.as_ref(),
						"SELECT",
						"the index name is ambiguous; qualify its source",
					));
				}
			}
		};
		hints.entry(source).or_default().insert(hint.index.clone());
	}
	Ok(hints)
}

#[allow(clippy::too_many_arguments, reason = "source planning owns one bounded candidate set")]
pub(super) fn plan_source(
	source: &LogicalSelectSource,
	offset: usize,
	logical: &LogicalSelect,
	snapshot: &PlannerSnapshot,
	forced: Option<&BTreeSet<Box<str>>>,
	no_index: bool,
	weights: CostWeights,
) -> FerrosGrynnResult<(SourceOperator, Cardinality, PlanCost, Vec<PlanWarning>)> {
	let baseline = source_cardinality(source, snapshot)?;
	let (mut operator, random) = match &source.kind {
		LogicalSelectSourceKind::Atoms {
			target: ScanTarget::Record(record),
			..
		} => (SourceOperator::PointGet(record.clone()), true),
		LogicalSelectSourceKind::Atoms {
			target: ScanTarget::Class(class),
			graph,
		} => match graph {
			None => (
				SourceOperator::ClassScan {
					class: class.clone(),
				},
				false,
			),
			Some(LogicalGraphReference::Static(graph)) => (
				SourceOperator::GraphClassScan {
					class: class.clone(),
					graph: Some(graph.id()),
				},
				false,
			),
			Some(LogicalGraphReference::Expression(_)) => (
				SourceOperator::GraphClassScan {
					class: class.clone(),
					graph: None,
				},
				false,
			),
		},
		LogicalSelectSourceKind::Graphs {
			..
		} => (SourceOperator::GraphCatalogScan, false),
		LogicalSelectSourceKind::Derived(_) => (SourceOperator::Derived, false),
	};
	let mut cardinality = baseline;
	let mut cost = if random {
		lookup_cost(1, cardinality, weights)
	} else {
		scan_cost(cardinality, 0, weights)
	};
	let mut warnings = Vec::new();
	let Some(class_name) = source_class(source) else {
		if forced.is_some() {
			return Err(index_hint_error(
				"INDEX",
				source.alias.as_ref(),
				"index hints apply only to atom-class sources",
			));
		}
		return Ok((operator, cardinality, cost, warnings));
	};
	if no_index
		|| matches!(source.kind, LogicalSelectSourceKind::Atoms {
			target: ScanTarget::Record(_),
			..
		}) {
		return Ok((operator, cardinality, cost, warnings));
	}
	let class = snapshot
		.class(class_name)
		.ok_or_else(|| FerrosGrynnError::class_does_not_exist(class_name.to_string()))?;
	let expected_scope = source_scope(source);
	let count_rewrite = count_rewrite(logical) && offset == 0;
	let mut candidates = Vec::new();
	for index in &class.indexes {
		let named = forced.is_none_or(|forced| forced.contains(index.name.as_str()));
		if !named {
			continue;
		}
		validate_forced_index(index, source.alias.as_ref(), expected_scope, forced.is_some())?;
		if index.state != ClassIndexState::Ready
			|| !index.physical_binding
			|| Some(index.scope) != expected_scope
		{
			if forced.is_none() {
				let reason = if index.state != ClassIndexState::Ready {
					format!("{}: state is {:?}", index.name, index.state)
				} else if !index.physical_binding {
					format!("{}: physical binding is absent", index.name)
				} else {
					format!("{}: scope does not match source {}", index.name, source.alias)
				};
				warnings.push(PlanWarning::IndexUnavailable(reason.into_boxed_str()));
			}
			continue;
		}
		if index.kind == ClassIndexKind::Count {
			if count_rewrite && index.exact_count.is_some() {
				let rows = Cardinality::exact(1);
				candidates.push((
					lookup_cost(1, rows, weights),
					index.id,
					SourceOperator::CountLookup {
						index: index.id,
						name: index.name.as_str().into(),
					},
					rows,
					false,
				));
			}
			continue;
		}
		let access = scalar_access_kind(source, offset, logical.filter.as_ref(), index);
		let ordering = index_ordering(logical, source, offset, index, class);
		let (access, direction) = match (access, ordering, forced.is_some()) {
			(Some(access), ordering, _) => {
				(access, ordering.map_or(IndexDirection::Forward, |ordering| ordering.direction))
			}
			(None, Some(ordering), _) => (IndexAccessKind::Full, ordering.direction),
			(None, None, true) => (IndexAccessKind::Full, IndexDirection::Forward),
			(None, None, false) => continue,
		};
		let estimate = if index.kind == ClassIndexKind::Unique && access == IndexAccessKind::Exact {
			Cardinality::new(0, 1, 1)
		} else {
			estimate_predicate(logical, snapshot, baseline, logical.filter.as_ref()).0
		};
		let satisfies_order = ordering.is_some();
		let candidate_cost = match access {
			IndexAccessKind::Exact => lookup_cost(1, estimate, weights),
			IndexAccessKind::Range => range_lookup_cost(1, estimate, weights),
			IndexAccessKind::Full => scan_cost(estimate, 0, weights),
		};
		candidates.push((
			candidate_cost,
			index.id,
			SourceOperator::ScalarIndexScan {
				index: index.id,
				name: index.name.as_str().into(),
				access,
				direction,
			},
			estimate,
			satisfies_order,
		));
	}
	if let Some(forced) = forced {
		for name in forced {
			if !class.indexes.iter().any(|index| index.name.as_str() == name.as_ref()) {
				return Err(index_hint_error(
					name,
					source.alias.as_ref(),
					"the index does not exist on the source class",
				));
			}
		}
		if candidates.is_empty() {
			return Err(index_hint_error(
				forced.iter().next().map_or("INDEX", Box::as_ref),
				source.alias.as_ref(),
				"no requested index is compatible with the source predicate or aggregate",
			));
		}
	}
	candidates.sort_by_key(|(candidate_cost, id, _, _, _)| (candidate_cost.total(), id.get()));
	if let Some((candidate_cost, _id, candidate, candidate_cardinality, satisfies_order)) =
		candidates.into_iter().next()
		&& (forced.is_some() || satisfies_order || candidate_cost.total() < cost.total())
	{
		cost = candidate_cost;
		operator = candidate;
		cardinality = candidate_cardinality;
	}
	Ok((operator, cardinality, cost, warnings))
}

fn validate_forced_index(
	index: &PlannerIndex,
	source: &str,
	expected_scope: Option<IndexScope>,
	forced: bool,
) -> FerrosGrynnResult<()> {
	if !forced {
		return Ok(());
	}
	if index.state != ClassIndexState::Ready {
		return Err(index_hint_error(index.name.as_str(), source, "the index is not Ready"));
	}
	if !index.physical_binding {
		return Err(index_hint_error(
			index.name.as_str(),
			source,
			"there is no transaction-visible physical binding",
		));
	}
	if Some(index.scope) != expected_scope {
		return Err(index_hint_error(index.name.as_str(), source, "the index has the wrong scope"));
	}
	Ok(())
}

fn index_hint_error(index: &str, source: &str, reason: &str) -> FerrosGrynnError {
	FerrosGrynnError::index_hint_not_usable(index, source, reason)
}

fn scalar_access_kind(
	source: &LogicalSelectSource,
	offset: usize,
	filter: Option<&TypedExpr>,
	index: &PlannerIndex,
) -> Option<IndexAccessKind> {
	let mut constraints = BTreeMap::new();
	collect_constraint_shapes(filter?, &mut constraints);
	let mut matched = 0;
	let mut range = false;
	for field in &index.fields {
		let local = source.schema.index_of(field.as_str())?;
		match constraints.get(&(offset + local)) {
			Some(ConstraintShape::Equality | ConstraintShape::In) if !range => matched += 1,
			Some(ConstraintShape::Range) if !range => {
				matched += 1;
				range = true;
				break;
			}
			_ => break,
		}
	}
	if matched == 0 {
		return None;
	}
	if matched == index.fields.len() && !range {
		Some(IndexAccessKind::Exact)
	} else {
		Some(IndexAccessKind::Range)
	}
}

#[derive(Clone, Copy)]
struct IndexOrdering {
	direction: IndexDirection,
}

fn index_ordering(
	logical: &LogicalSelect,
	source: &LogicalSelectSource,
	offset: usize,
	index: &PlannerIndex,
	class: &super::super::PlannerClass,
) -> Option<IndexOrdering> {
	if logical.joins.is_empty()
		&& logical.group.is_none()
		&& logical.split.is_empty()
		&& !logical.order.is_empty()
		&& logical.order.len() == index.fields.len()
	{
		let direction = match logical.order.first()?.direction {
			crate::syn::SelectOrderDirection::Ascending => IndexDirection::Forward,
			crate::syn::SelectOrderDirection::Descending => IndexDirection::Reverse,
		};
		for (order, field) in logical.order.iter().zip(&index.fields) {
			if order.random {
				return None;
			}
			let Some(column) = order_source_column(logical, &order.expression) else {
				return None;
			};
			if column != offset + source.schema.index_of(field.as_str())? {
				return None;
			}
			let same_direction = match direction {
				IndexDirection::Forward => {
					order.direction == crate::syn::SelectOrderDirection::Ascending
				}
				IndexDirection::Reverse => {
					order.direction == crate::syn::SelectOrderDirection::Descending
				}
			};
			if !same_direction {
				return None;
			}
			let field = class.fields.iter().find(|candidate| candidate.name == *field)?;
			let nullable = matches!(
				field.value_type,
				crate::values::Type::Any
					| crate::values::Type::None
					| crate::values::Type::Null
					| crate::values::Type::Option(_)
					| crate::values::Type::Union(_)
			);
			if nullable
				&& !(direction == IndexDirection::Forward
					&& order.nulls == crate::syn::SelectNulls::First)
			{
				return None;
			}
		}
		Some(IndexOrdering {
			direction,
		})
	} else {
		None
	}
}

fn order_source_column(logical: &LogicalSelect, expression: &TypedExpr) -> Option<usize> {
	let TypedExprKind::Column(column) = expression.kind else {
		return None;
	};
	if column < logical.input_schema.columns.len() {
		return Some(column);
	}
	let target = column.checked_sub(logical.input_schema.columns.len())?;
	let mut output = 0_usize;
	for item in &logical.projection {
		match item {
			LogicalSelectItem::Wildcard(fields) => {
				for field in fields {
					if output == target {
						return match field {
							LogicalWildcardField::Column {
								index,
								..
							} => Some(*index),
							LogicalWildcardField::Properties {
								..
							} => None,
						};
					}
					output = output.checked_add(1)?;
				}
			}
			LogicalSelectItem::Expression {
				expression,
				..
			} => {
				if output == target {
					return match expression.kind {
						TypedExprKind::Column(source) => Some(source),
						_ => None,
					};
				}
				output = output.checked_add(1)?;
			}
		}
	}
	None
}

pub(super) fn sort_eliminated(
	logical: &LogicalSelect,
	snapshot: &PlannerSnapshot,
	sources: &[PhysicalSource],
) -> bool {
	let Some(PhysicalSource {
		operator: SourceOperator::ScalarIndexScan {
			index,
			..
		},
		..
	}) = sources.first()
	else {
		return false;
	};
	let Some(class_name) = source_class(&logical.source) else {
		return false;
	};
	let Some(class) = snapshot.class(class_name) else {
		return false;
	};
	let Some(index) = class.indexes.iter().find(|candidate| candidate.id == *index) else {
		return false;
	};
	index_ordering(logical, &logical.source, 0, index, class).is_some()
}

#[derive(Clone, Copy)]
enum ConstraintShape {
	Equality,
	In,
	Range,
}

fn collect_constraint_shapes(
	expression: &TypedExpr,
	output: &mut BTreeMap<usize, ConstraintShape>,
) {
	let TypedExprKind::Binary {
		left,
		op,
		right,
	} = &expression.kind
	else {
		return;
	};
	if *op == BinaryOperator::And {
		collect_constraint_shapes(left, output);
		collect_constraint_shapes(right, output);
		return;
	}
	let shape = match op {
		BinaryOperator::Equal | BinaryOperator::Is => ConstraintShape::Equality,
		BinaryOperator::In => ConstraintShape::In,
		BinaryOperator::Less
		| BinaryOperator::LessOrEqual
		| BinaryOperator::Greater
		| BinaryOperator::GreaterOrEqual => ConstraintShape::Range,
		_ => return,
	};
	let column = if *op == BinaryOperator::In {
		match &left.kind {
			TypedExprKind::Column(column) => *column,
			_ => return,
		}
	} else {
		match (&left.kind, &right.kind) {
			(
				TypedExprKind::Column(column),
				TypedExprKind::Constant(_) | TypedExprKind::Slot(_),
			)
			| (
				TypedExprKind::Constant(_) | TypedExprKind::Slot(_),
				TypedExprKind::Column(column),
			) => *column,
			_ => return,
		}
	};
	output.insert(column, shape);
}

pub(super) fn source_cardinality(
	source: &LogicalSelectSource,
	snapshot: &PlannerSnapshot,
) -> FerrosGrynnResult<Cardinality> {
	Ok(match &source.kind {
		LogicalSelectSourceKind::Atoms {
			target: ScanTarget::Record(_),
			..
		} => Cardinality::new(0, 1, 1),
		LogicalSelectSourceKind::Atoms {
			target: ScanTarget::Class(class),
			graph,
		} => {
			let metadata = snapshot
				.class(class)
				.ok_or_else(|| FerrosGrynnError::class_does_not_exist(class.to_string()))?;
			match graph {
				None => Cardinality::exact(metadata.base_rows),
				Some(LogicalGraphReference::Static(graph)) => {
					Cardinality::exact(metadata.graph_rows.get(&graph.id()).copied().unwrap_or(0))
				}
				Some(LogicalGraphReference::Expression(_)) => {
					Cardinality::unknown(metadata.base_rows)
				}
			}
		}
		LogicalSelectSourceKind::Graphs {
			references,
		} => Cardinality::exact(
			u64::try_from(references.as_ref().map_or(snapshot.graphs.len(), Vec::len))
				.unwrap_or(u64::MAX),
		),
		LogicalSelectSourceKind::Derived(select) => {
			plan_select(select, snapshot, usize::MAX)?.cardinality
		}
	})
}

fn source_class(source: &LogicalSelectSource) -> Option<&str> {
	match &source.kind {
		LogicalSelectSourceKind::Atoms {
			target,
			..
		} => Some(target.class()),
		LogicalSelectSourceKind::Graphs {
			..
		}
		| LogicalSelectSourceKind::Derived(_) => None,
	}
}

fn source_scope(source: &LogicalSelectSource) -> Option<IndexScope> {
	match &source.kind {
		LogicalSelectSourceKind::Atoms {
			graph: None,
			..
		} => Some(IndexScope::Base),
		LogicalSelectSourceKind::Atoms {
			graph: Some(LogicalGraphReference::Static(graph)),
			..
		} => Some(IndexScope::Effective {
			graph: graph.id(),
		}),
		LogicalSelectSourceKind::Atoms {
			graph: Some(LogicalGraphReference::Expression(_)),
			..
		}
		| LogicalSelectSourceKind::Graphs {
			..
		}
		| LogicalSelectSourceKind::Derived(_) => None,
	}
}

pub(super) fn index_join_probe(
	source: &LogicalSelectSource,
	source_index: usize,
	keys: &[(usize, usize)],
	offsets: &[usize],
	snapshot: &PlannerSnapshot,
	forced: Option<&BTreeSet<Box<str>>>,
) -> Option<(IndexJoinProbe, Box<str>)> {
	let class = snapshot.class(source_class(source)?)?;
	let scope = source_scope(source)?;
	let mut by_right = BTreeMap::new();
	for (left, right) in keys {
		by_right.insert(*right, *left);
	}
	let mut candidates = class
		.indexes
		.iter()
		.filter(|index| {
			index.state == ClassIndexState::Ready
				&& index.physical_binding
				&& index.scope == scope
				&& matches!(index.kind, ClassIndexKind::Standard | ClassIndexKind::Unique)
				&& forced.is_none_or(|forced| forced.contains(index.name.as_str()))
		})
		.filter_map(|index| {
			let left_columns = index
				.fields
				.iter()
				.map(|field| {
					let local = source.schema.index_of(field.as_str())?;
					by_right.get(&(offsets[source_index] + local)).copied()
				})
				.collect::<Option<Vec<_>>>()?;
			Some((index.id, index.name.as_str().into(), left_columns))
		})
		.collect::<Vec<_>>();
	candidates.sort_by_key(|(index, _name, columns)| (columns.len(), index.get()));
	let (index, name, left_columns) = candidates.into_iter().next()?;
	Some((
		IndexJoinProbe {
			index,
			left_columns,
		},
		name,
	))
}

fn count_rewrite(select: &LogicalSelect) -> bool {
	select.joins.is_empty()
		&& select.filter.is_none()
		&& select.split.is_empty()
		&& select.order.is_empty()
		&& select.omit.is_empty()
		&& select.fetch.is_empty()
		&& select.group.as_ref().is_some_and(Vec::is_empty)
		&& matches!(select.projection.as_slice(), [LogicalSelectItem::Expression {
			expression: TypedExpr {
				kind: TypedExprKind::Aggregate {
					function: Aggregate::Count,
					argument: None,
					distinct: false,
				},
				..
			},
			..
		}])
}

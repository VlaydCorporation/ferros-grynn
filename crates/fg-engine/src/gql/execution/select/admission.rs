//! Bounded copies include nested SELECT trees rather than treating them as
//! leaves.

use super::*;

/// Checks a copied typed tree, including independently lowered source plans.
pub(super) fn expression_copy<Read: QueryRead>(
	read: &Read,
	expression: &TypedExpr,
	depth: usize,
) -> FerrosGrynnResult<()> {
	check_depth(depth)?;
	crate::gql::analysis::visit_typed_expression(expression, &mut |node| {
		admit_special_copy(read, node)?;
		admit_layout_copy(read, node)?;
		if let TypedExprKind::Subquery {
			select,
			..
		} = &node.kind
		{
			select_copy(read, select, depth + 1)?;
		}
		Ok(())
	})
}

/// Pre-admits plan copies made by the shared optimizer and union refinement.
pub(super) fn select_copy<Read: QueryRead>(
	read: &Read,
	plan: &LogicalSelect,
	depth: usize,
) -> FerrosGrynnResult<()> {
	check_depth(depth)?;
	let Some(fields) = read.bounded_fields() else {
		return Ok(());
	};
	fields.work(1)?;
	fields.admit(4 * std::mem::size_of::<LogicalSelect>())?;
	for schema in [&plan.source.schema, &plan.input_schema, &plan.result] {
		schema_copy(read, schema)?;
	}
	source_copy(read, &plan.source, depth)?;
	fields.admit(
		plan.joins
			.len()
			.checked_mul(4 * std::mem::size_of::<crate::gql::ir::LogicalSelectJoin>())
			.ok_or_else(overflow)?,
	)?;
	for join in &plan.joins {
		fields.work(1)?;
		schema_copy(read, &join.source.schema)?;
		source_copy(read, &join.source, depth)?;
		wildcard_copy(read, &join.source.wildcard)?;
		if let Some(predicate) = &join.predicate {
			expression_copy(read, predicate, depth)?;
		}
	}
	for input in &plan.union_inputs {
		select_copy(read, input, depth + 1)?;
	}
	for item in &plan.projection {
		match item {
			LogicalSelectItem::Expression {
				name,
				expression,
			} => {
				fields.admit(
					name.len()
						.checked_mul(4)
						.and_then(|bytes| {
							bytes.checked_add(4 * std::mem::size_of::<LogicalSelectItem>())
						})
						.ok_or_else(overflow)?,
				)?;
				expression_copy(read, expression, depth)?;
			}
			LogicalSelectItem::WildcardVariants {
				selector,
				choices,
			} => {
				expression_copy(read, selector, depth)?;
				fields.admit(
					choices
						.len()
						.checked_mul(std::mem::size_of::<Vec<LogicalWildcardField>>())
						.ok_or_else(overflow)?,
				)?;
				for choice in choices {
					wildcard_copy(read, choice)?;
				}
			}
			LogicalSelectItem::Wildcard(wildcard) => wildcard_copy(read, wildcard)?,
		}
	}
	wildcard_copy(read, &plan.source.wildcard)?;
	for expression in plan
		.filter
		.iter()
		.chain(plan.split.iter())
		.chain(plan.group.iter().flatten())
		.chain(plan.limit.iter())
	{
		expression_copy(read, expression, depth)?;
	}
	for order in &plan.order {
		expression_copy(read, &order.expression, depth)?;
	}
	if let Some(graph) = &plan.return_graph {
		fields.admit(4 * std::mem::size_of::<crate::gql::ir::LogicalReturnGraph>())?;
		for expression in graph.expressions() {
			expression_copy(read, expression, depth)?;
		}
	}
	if let Some(start) = &plan.start {
		let expression = match start {
			LogicalSelectStart::Offset(expression) => expression,
			LogicalSelectStart::After {
				cursor,
				..
			} => cursor,
		};
		expression_copy(read, expression, depth)?;
	}
	for path in
		plan.fetch.iter().chain(plan.omit.iter()).chain(plan.projection_paths.iter().flatten())
	{
		fields.admit(
			path.len().checked_mul(4 * std::mem::size_of::<Box<str>>()).ok_or_else(overflow)?,
		)?;
		for name in path {
			fields.work(1)?;
			fields.admit(name.len().checked_mul(4).ok_or_else(overflow)?)?;
		}
	}
	Ok(())
}

/// Schemas retain names, type shapes and source origins through optimizer
/// copies.
pub(super) fn schema_copy<Read: QueryRead>(
	read: &Read,
	schema: &crate::gql::ir::Schema,
) -> FerrosGrynnResult<()> {
	let Some(fields) = read.bounded_fields() else {
		return Ok(());
	};
	fields.admit(
		schema
			.columns
			.len()
			.checked_mul(4 * std::mem::size_of::<crate::gql::ir::Column>())
			.ok_or_else(overflow)?,
	)?;
	for column in &schema.columns {
		fields.work(1)?;
		fields.admit(
			column
				.name
				.len()
				.checked_add(column.qualifier.as_ref().map_or(0, |name| name.len()))
				.and_then(|bytes| bytes.checked_mul(4))
				.ok_or_else(overflow)?,
		)?;
		crate::gql::live_lowering::admit_type_shape(&column.ty, 0, 4, &mut |bytes, work| {
			fields.work(work)?;
			fields.admit(bytes)
		})?;
	}
	for origin in schema.origins.values() {
		fields.work(1)?;
		fields.admit(
			origin
				.source
				.len()
				.checked_add(origin.class.len())
				.and_then(|bytes| bytes.checked_add(origin.field.len()))
				.and_then(|bytes| bytes.checked_mul(4))
				.and_then(|bytes| bytes.checked_add(512))
				.ok_or_else(overflow)?,
		)?;
	}
	Ok(())
}

/// A source retains its alias, target and graph expression independently of
/// projection.
fn source_copy<Read: QueryRead>(
	read: &Read,
	source: &LogicalSelectSource,
	depth: usize,
) -> FerrosGrynnResult<()> {
	let Some(fields) = read.bounded_fields() else {
		return Ok(());
	};
	fields.admit(source.alias.len().checked_mul(4).ok_or_else(overflow)?)?;
	match &source.kind {
		LogicalSelectSourceKind::Empty {
			graph,
		} => {
			if let Some(graph) = graph {
				graph_copy(read, graph, depth)?;
			}
		}
		LogicalSelectSourceKind::Atoms {
			target,
			graph,
		} => {
			fields.admit(target.class().len().checked_mul(4).ok_or_else(overflow)?)?;
			if let ScanTarget::Record(record) = target {
				fields.admit(record.estimated_heap_bytes().checked_mul(4).ok_or_else(overflow)?)?;
			}
			if let Some(graph) = graph {
				graph_copy(read, graph, depth)?;
			}
		}
		LogicalSelectSourceKind::Graphs {
			references,
		} => {
			if let Some(references) = references {
				for graph in references {
					graph_copy(read, graph, depth)?;
				}
			}
		}
		LogicalSelectSourceKind::AtomUnion {
			targets,
			graph,
			..
		} => {
			fields.work(targets.len() as u64)?;
			fields.admit(
				targets
					.len()
					.checked_mul(4 * std::mem::size_of::<crate::gql::ir::ScanTarget>())
					.ok_or_else(overflow)?,
			)?;
			for target in targets {
				fields.admit(target.class().len().checked_mul(4).ok_or_else(overflow)?)?;
			}
			if let Some(graph) = graph {
				graph_copy(read, graph, depth)?;
			}
		}
		LogicalSelectSourceKind::Derived(inner) => select_copy(read, inner, depth + 1)?,
	}
	Ok(())
}

/// Source graph values preserve owned names and nested expressions.
pub(super) fn graph_copy<Read: QueryRead>(
	read: &Read,
	graph: &crate::gql::ir::LogicalGraphReference,
	depth: usize,
) -> FerrosGrynnResult<()> {
	match graph {
		crate::gql::ir::LogicalGraphReference::Static(graph) => {
			if let Some(fields) = read.bounded_fields() {
				fields.work(1)?;
				fields.admit(
					graph
						.name()
						.len()
						.checked_mul(4)
						.and_then(|bytes| bytes.checked_add(4 * std::mem::size_of::<GraphRef>()))
						.ok_or_else(overflow)?,
				)?;
			}
			Ok(())
		}
		crate::gql::ir::LogicalGraphReference::Expression(expression) => {
			expression_copy(read, expression, depth)
		}
	}
}

/// Wildcard names and containers belong to the copied source/projection.
pub(super) fn wildcard_copy<Read: QueryRead>(
	read: &Read,
	wildcard: &[crate::gql::ir::LogicalWildcardField],
) -> FerrosGrynnResult<()> {
	if let Some(fields) = read.bounded_fields() {
		fields.work(wildcard.len() as u64)?;
		fields.admit(
			wildcard
				.len()
				.checked_mul(4 * std::mem::size_of::<crate::gql::ir::LogicalWildcardField>())
				.ok_or_else(overflow)?,
		)?;
		for field in wildcard {
			if let crate::gql::ir::LogicalWildcardField::Column {
				name,
				..
			} = field
			{
				fields.admit(name.len().checked_mul(4).ok_or_else(overflow)?)?;
			}
		}
	}
	Ok(())
}

/// SDK syntax is bounded; synthesized wrappers still have a hard stack cap.
fn check_depth(depth: usize) -> FerrosGrynnResult<()> {
	if depth > 256 {
		return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			"LIVE typed SELECT depth",
			256,
		));
	}
	Ok(())
}

/// Arithmetic failure occurs before copied buffers or names are allocated.
fn overflow() -> FerrosGrynnError {
	FerrosGrynnError::operation_overflow("LIVE typed SELECT copy")
}

#[cfg(test)]
/// Regression tests for admission behavior.
mod tests {
	use std::num::NonZeroUsize;

	use super::*;
	use crate::{
		datastore::{ClassSchema, Datastore},
		gql::{
			ExecutionControl,
			QueryExecutionOptions,
			analysis,
			execution::RootRead,
			ir::LogicalStatement,
		},
		graph::AtomKind,
		syn::{ParserSettings, parse},
		values::Object,
	};

	/// Child admission alone cannot cover copied cold graph-selector storage.
	#[test]
	fn select_copy_admits_boxed_graph_shell_before_cloning() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let mut setup = store.begin();
		setup.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
		setup.commit().assert_ok();
		let root = store.inner.current_root();
		let options = QueryExecutionOptions::default();
		let control = ExecutionControl::new(&options, None);
		let observe = |_| {};
		let work = |_| Ok(());
		let read = RootRead::new(&root, &control, &observe, &work, std::env::temp_dir());
		let source = "SELECT 1 AS picked FROM item RETURN GRAPH {nodes: [], edges: []};";
		let syntax = parse(source, ParserSettings::default()).assert_ok();
		let logical = analysis::analyze(&syntax, source, &read, &Object::new()).assert_ok();
		let Some(LogicalStatement::Select(mut plan)) = logical.statements.into_iter().next() else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("SELECT admission fixture absent"));
				test_result.assert_ok();
			};
		};
		let graph = plan
			.return_graph
			.take()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("graph admission fixture absent"))
			.assert_ok();
		let calibration = ExecutionControl::new(&options, None);
		let calibration_read =
			RootRead::new(&root, &calibration, &observe, &work, std::env::temp_dir());
		select_copy(&calibration_read, &plan, 0).assert_ok();
		for expression in graph.expressions() {
			expression_copy(&calibration_read, expression, 0).assert_ok();
		}
		let children_only =
			calibration.barrier_memory_limit() - calibration.barrier_memory_available();
		let options = QueryExecutionOptions {
			barrier_memory_bytes: NonZeroUsize::new(children_only)
				.ok_or_else(|| {
					FerrosGrynnError::invalid_argument("admission calibration was empty")
				})
				.assert_ok(),
			..options
		};
		plan.return_graph = Some(graph);
		let limited = ExecutionControl::new(&options, None);
		let limited_read = RootRead::new(&root, &limited, &observe, &work, std::env::temp_dir());
		assert!(select_copy(&limited_read, &plan, 0).is_err());
	}
}

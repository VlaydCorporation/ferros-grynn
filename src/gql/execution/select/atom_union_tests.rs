//! Raw union rows preserve class-local positions before relational dispatch.

use super::*;
use crate::{
	datastore::{ClassField, ClassSchema, Datastore},
	gql::{ParserSettings, QueryExecutionOptions},
	graph::{AtomKind, GraphKind},
	syn::{Identifier, SelectSourceKind, StatementKind},
	values::{Object, PropertyMap, RecordId, Type},
};

/// Width, private tags, scope and source estimates agree with the raw oracle.
#[test]
fn live_atom_union_layout_scope_and_scan() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("a", AtomKind::Node, [
		ClassField::new("alpha", Type::String),
		ClassField::new("value", Type::Int),
	])?)?;
	setup.define_class(ClassSchema::new("b", AtomKind::Node, [ClassField::new(
		"value",
		Type::Float,
	)])?)?;
	let value = setup.intern_property("value")?;
	let alpha = setup.intern_property("alpha")?;
	let a: RecordId = "a:one".parse()?;
	let b: RecordId = "b:one".parse()?;
	setup.create_atom(
		a.clone(),
		PropertyMap::from_iter([(value, 11.into()), (alpha, "x".into())]),
	)?;
	setup.create_atom(b.clone(), PropertyMap::from_iter([(value, Value::from(2.5_f64))]))?;
	let graph = setup.define_graph("union_scope", GraphKind::Graph)?;
	setup.attach_node(graph.id(), &a, None, None)?;
	setup.attach_node(graph.id(), &b, None, None)?;
	setup.set_overlay(graph.id(), &a, value, 12.into())?;
	setup.create_atom(
		"b:outside".parse()?,
		PropertyMap::from_iter([(value, Value::from(99.0_f64))]),
	)?;
	setup.commit()?;
	let root = store.inner.current_root();
	let control = ExecutionControl::new(&QueryExecutionOptions::default(), None);
	let read = RootRead::new(&root, &control, &|_| {}, &|_| Ok(()), std::env::temp_dir());
	let mut program = crate::gql::parse(
		"SELECT VALUE id FROM a IN GRAPH union_scope AS u;",
		ParserSettings::default(),
	)?;
	let Some(statement) = program.statements.first_mut() else {
		return Err(FerrosGrynnError::invalid_argument("union fixture statement missing"));
	};
	let StatementKind::Select(select) = &mut statement.kind else {
		return Err(FerrosGrynnError::invalid_argument("union fixture SELECT missing"));
	};
	select.union_purpose = crate::syn::SelectUnionPurpose::JoinedTypingVariants;
	select.source.kind = SelectSourceKind::AtomUnion {
		classes: ["a", "b"]
			.map(|name| Identifier {
				value: name.into(),
				quoted: true,
				span: crate::gql_status::TextSpan::new(0, 0),
			})
			.into(),
		typing: Some(0),
	};
	let parameters = Object::new();
	let typed = crate::gql::analysis::analyze(&program, "", &read, &parameters)?;
	let Some(LogicalStatement::Select(plan)) = typed.statements.first() else {
		return Err(FerrosGrynnError::invalid_argument("union fixture typed SELECT missing"));
	};
	let LogicalSelectSourceKind::AtomUnion {
		tag_column,
		..
	} = &plan.source.kind
	else {
		return Err(FerrosGrynnError::invalid_argument("union fixture source kind missing"));
	};
	assert_eq!(plan.source.schema.columns.len(), 8);
	assert_eq!(*tag_column, 7);
	assert_eq!(plan.source.schema.index_of("value"), Some(6));
	assert_eq!(plan.source.schema.index_of("\0fg_class"), None);
	assert_eq!(plan.source.schema.qualified_index("u", "\0fg_class"), None);
	assert_eq!(plan.source.schema.matching_indices("\0fg_class").count(), 0);
	let snapshot = read.snapshot()?;
	let (_, physical) = prepare_physical_plan(&read, snapshot, plan, &control)?;
	assert_eq!(physical.cardinality.expected, 2);
	assert!(matches!(
		physical.sources[0].operator,
		crate::gql::optimizer::SourceOperator::AtomUnion { .. }
	));
	let rows = source_rows(
		&read,
		snapshot,
		&plan.source,
		None,
		None,
		None,
		&[],
		"",
		&control,
		None,
		None,
		None,
	)?;
	let mut observed = Vec::new();
	rows.consume(|row| {
		observed.push(row);
		Ok(())
	})?;
	assert_eq!(observed.len(), 2);
	assert_eq!(observed[0].value(0), Some(&Value::from(a)));
	assert_eq!(observed[0].value(6), Some(&Value::from(12)));
	assert_eq!(observed[0].value(7), Some(&Value::from(0)));
	assert_eq!(observed[1].value(0), Some(&Value::from(b)));
	assert_eq!(observed[1].value(5), Some(&Value::from(2.5_f64)));
	assert_eq!(observed[1].value(6), Some(&Value::None));
	assert_eq!(observed[1].value(7), Some(&Value::from(1)));
	assert!(
		observed.iter().all(|row| row.graph.as_ref().is_some_and(|scope| scope.id() == graph.id()))
	);
	Ok(())
}

/// Layout bounds, complete buffers and copied names fail before allocation.
#[test]
fn live_atom_union_source_layout_admission() -> FerrosGrynnResult<()> {
	use std::num::NonZeroUsize;
	let store = Datastore::in_memory();
	let root = store.inner.current_root();
	let mut options = QueryExecutionOptions::default();
	options.barrier_memory_bytes = NonZeroUsize::new(512)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture budget is zero"))?;
	let control = ExecutionControl::new(&options, None);
	let read = RootRead::new(&root, &control, &|_| {}, &|_| Ok(()), std::env::temp_dir());
	assert!(
		scoped_layout_row(&read, Row::new(vec![Value::None; 8]), None, None, Some((0, 8, 4096)))
			.is_err()
	);
	for layout in [(usize::MAX, 8, usize::MAX), (4, 8, 10), (0, 7, 8)] {
		assert!(layout_row(&read, Row::new(vec![Value::None; 8]), Some(layout)).is_err());
	}
	let mut options = QueryExecutionOptions::default();
	options.barrier_memory_bytes = NonZeroUsize::new(1)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture budget is zero"))?;
	let control = ExecutionControl::new(&options, None);
	let read = RootRead::new(&root, &control, &|_| {}, &|_| Ok(()), std::env::temp_dir());
	let graph = GraphRef::new(
		crate::graph::GraphId::new(1)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture graph is zero"))?,
		"scope_name",
		GraphKind::Graph,
		crate::graph::GraphStorageMode::InMemory,
	)?;
	assert!(scoped_layout_row(&read, Row::new(Vec::new()), Some(&graph), None, None).is_err());
	assert!(scoped_layout_row(&read, Row::new(Vec::new()), None, Some(&[0, 1]), None).is_err());
	let original = Row::new(vec![Value::None; 8]);
	let allocation = original.values.as_ptr();
	let unchanged = layout_row(&read, original, Some((0, 8, 8)))?;
	assert_eq!(unchanged.values.as_ptr(), allocation);
	Ok(())
}

//! Raw union rows preserve class-local positions before relational dispatch.

use fg_syntax::__internal::SelectMetadata as _;

use super::*;
use crate::{
	datastore::{ClassField, ClassSchema, Datastore},
	gql::{ParserSettings, QueryExecutionOptions},
	graph::{AtomKind, GraphKind},
	syn::{SelectSourceKind, StatementKind},
	values::{Object, PropertyMap, RecordId, Type},
};

/// Width, private tags, scope and source estimates agree with the raw oracle.
#[test]
fn live_atom_union_layout_scope_and_scan() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup
		.define_class(
			ClassSchema::new("a", AtomKind::Node, [
				ClassField::new("alpha", Type::String),
				ClassField::new("value", Type::Int),
			])
			.assert_ok(),
		)
		.assert_ok();
	setup
		.define_class(
			ClassSchema::new("b", AtomKind::Node, [ClassField::new("value", Type::Float)])
				.assert_ok(),
		)
		.assert_ok();
	let value = setup.intern_property("value").assert_ok();
	let alpha = setup.intern_property("alpha").assert_ok();
	let a: RecordId = "a:one".parse().assert_ok();
	let b: RecordId = "b:one".parse().assert_ok();
	setup
		.create_atom(a.clone(), PropertyMap::from_iter([(value, 11.into()), (alpha, "x".into())]))
		.assert_ok();
	setup
		.create_atom(b.clone(), PropertyMap::from_iter([(value, Value::from(2.5_f64))]))
		.assert_ok();
	let graph = setup.define_graph("union_scope", GraphKind::Graph).assert_ok();
	setup.attach_node(graph.id(), &a, None, None).assert_ok();
	setup.attach_node(graph.id(), &b, None, None).assert_ok();
	setup.set_overlay(graph.id(), &a, value, 12.into()).assert_ok();
	setup
		.create_atom(
			"b:outside".parse().assert_ok(),
			PropertyMap::from_iter([(value, Value::from(99.0_f64))]),
		)
		.assert_ok();
	setup.commit().assert_ok();
	let root = store.inner.current_root();
	let control = ExecutionControl::new(&QueryExecutionOptions::default(), None);
	let read = RootRead::new(&root, &control, &|_| {}, &|_| Ok(()), std::env::temp_dir());
	let mut program = crate::gql::parse(
		"SELECT VALUE id FROM a IN GRAPH union_scope AS u;",
		ParserSettings::default(),
	)
	.assert_ok();
	let Some(statement) = program.statements.first_mut() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("union fixture statement missing"));
			test_result.assert_ok();
		};
	};
	let StatementKind::Select(select) = &mut statement.kind else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("union fixture SELECT missing"));
			test_result.assert_ok();
		};
	};
	select.set_union_purpose(crate::syn::SelectUnionPurpose::JoinedTypingVariants);
	select.source.kind = SelectSourceKind::AtomUnion {
		classes: ["a", "b"]
			.map(|name| {
				let split_field_0 = name.into();
				let split_field_1 = true;
				let split_field_2 = crate::gql_status::TextSpan::new(0, 0);
				crate::syn::split_internal::new_identifier(
					split_field_0,
					split_field_1,
					split_field_2,
				)
			})
			.into(),
		typing: Some(0),
	};
	let parameters = Object::new();
	let typed = crate::gql::analysis::analyze(&program, "", &read, &parameters).assert_ok();
	let Some(LogicalStatement::Select(plan)) = typed.statements.first() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("union fixture typed SELECT missing"));
			test_result.assert_ok();
		};
	};
	let LogicalSelectSourceKind::AtomUnion {
		tag_column,
		..
	} = &plan.source.kind
	else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("union fixture source kind missing"));
			test_result.assert_ok();
		};
	};
	assert_eq!(plan.source.schema.columns.len(), 8);
	assert_eq!(*tag_column, 7);
	assert_eq!(plan.source.schema.index_of("value"), Some(6));
	assert_eq!(plan.source.schema.index_of("\0fg_class"), None);
	assert_eq!(plan.source.schema.qualified_index("u", "\0fg_class"), None);
	assert_eq!(plan.source.schema.matching_indices("\0fg_class").count(), 0);
	let snapshot = read.snapshot().assert_ok();
	let (_, physical) = prepare_physical_plan(&read, snapshot, plan, &control).assert_ok();
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
		SourceEvaluation {
			slots: &[],
			source: "",
			control: &control,
			outer: None,
		},
		None,
		None,
	)
	.assert_ok();
	let mut observed = Vec::new();
	rows.consume(|row| {
		observed.push(row);
		Ok(())
	})
	.assert_ok();
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
}

/// Layout bounds, complete buffers and copied names fail before allocation.
#[test]
fn live_atom_union_source_layout_admission() {
	use std::num::NonZeroUsize;

	use crate::test_assertions::AssertResult as _;
	let store = Datastore::in_memory();
	let root = store.inner.current_root();
	let options = QueryExecutionOptions {
		barrier_memory_bytes: NonZeroUsize::new(512)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture budget is zero"))
			.assert_ok(),
		..Default::default()
	};
	let control = ExecutionControl::new(&options, None);
	let read = RootRead::new(&root, &control, &|_| {}, &|_| Ok(()), std::env::temp_dir());
	assert!(
		scoped_layout_row(&read, Row::new(vec![Value::None; 8]), None, None, Some((0, 8, 4096)))
			.is_err()
	);
	for layout in [(usize::MAX, 8, usize::MAX), (4, 8, 10), (0, 7, 8)] {
		assert!(layout_row(&read, Row::new(vec![Value::None; 8]), Some(layout)).is_err());
	}
	let options = QueryExecutionOptions {
		barrier_memory_bytes: NonZeroUsize::new(1)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture budget is zero"))
			.assert_ok(),
		..Default::default()
	};
	let control = ExecutionControl::new(&options, None);
	let read = RootRead::new(&root, &control, &|_| {}, &|_| Ok(()), std::env::temp_dir());
	let graph = GraphRef::new(
		crate::graph::GraphId::new(1)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture graph is zero"))
			.assert_ok(),
		"scope_name",
		GraphKind::Graph,
		crate::graph::GraphStorageMode::InMemory,
	)
	.assert_ok();
	assert!(scoped_layout_row(&read, Row::new(Vec::new()), Some(&graph), None, None).is_err());
	assert!(scoped_layout_row(&read, Row::new(Vec::new()), None, Some(&[0, 1]), None).is_err());
	let original = Row::new(vec![Value::None; 8]);
	let allocation = original.values.as_ptr();
	let unchanged = layout_row(&read, original, Some((0, 8, 8))).assert_ok();
	assert_eq!(unchanged.values.as_ptr(), allocation);
}

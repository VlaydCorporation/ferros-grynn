//! Prepared MATCH copies reserve the whole new fixed-shell delta exactly once.

use std::{mem::size_of, num::NonZeroUsize};

use super::prepare_steps;
use crate::{
	datastore::{ClassSchema, Datastore},
	gql::{
		ExecutionControl,
		QueryExecutionOptions,
		analysis,
		execution::RootRead,
		ir::{LogicalMatchPatternKind, LogicalStatement, MatchBinding},
		root_catalog::RootCatalog,
	},
	gql_status::Condition,
	graph::{AtomKind, GraphKind},
	syn::{ParserSettings, parse},
	test_assertions::{AssertOption as _, AssertResult as _},
	values::Object,
};

/// A WITHIN owner and the MATCH counter word belong to each actual prepared
/// copy.
#[test]
fn prepared_traversal_match_within_admits_full_delta_once() {
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	for class in ["module", "document"] {
		setup.define_class(ClassSchema::new(class, AtomKind::Node, []).assert_ok()).assert_ok();
	}
	setup.define_graph("scope", GraphKind::Metagraph).assert_ok();
	setup.commit().assert_ok();
	let root = store.inner.current_root();
	let source = "TRAVERSE FROM [] |> tv::match(MATCH (document AS child) WITHIN <(module AS \
	              owner)> IN GRAPH scope RETURN child, owner);";
	let syntax = parse(source, ParserSettings::default()).assert_ok();
	let logical =
		analysis::analyze(&syntax, source, &RootCatalog(&root), &Object::new()).assert_ok();
	let plan = match logical.statements.into_iter().next() {
		Some(LogicalStatement::Traverse(plan)) => Some(plan),
		_ => None,
	}
	.assert_some_with("fixture must lower an ordinary traversal statement");
	assert_eq!(plan.steps.len(), 1);
	let matching = plan.steps[0].match_plan.as_ref().assert_some();
	assert!(matches!(matching.patterns[0].kind, LogicalMatchPatternKind::Within(_)));
	let delta = size_of::<MatchBinding>() + size_of::<usize>();
	assert_eq!(matching.boxed_layout_bytes, delta);
	for cap in [delta - 1, delta] {
		let options = QueryExecutionOptions {
			barrier_memory_bytes: NonZeroUsize::new(cap).assert_some(),
			..QueryExecutionOptions::default()
		};
		let control = ExecutionControl::new(&options, None);
		let read = RootRead::new(&root, &control, &|_| {}, &|_| Ok(()), std::env::temp_dir());
		let prepared = prepare_steps(&plan, None, &[], source, &control, read.fields());
		if cap < delta {
			let error = prepared.err().assert_some_with("the full copy delta must be reserved");
			assert_eq!(error.status().condition(), Condition::ProgramLimitExceeded);
			assert_eq!(control.barrier_memory_available(), cap);
		} else {
			let prepared = prepared.assert_ok();
			assert_eq!(prepared.len(), 1);
			assert_eq!(control.barrier_memory_available(), 0);
			assert_eq!(prepared[0].match_plan.as_ref().assert_some().boxed_layout_bytes, delta);
		}
	}
}

//! Fixed-shell accounting is independent of source spans and nesting depth.

use std::{collections::BTreeSet, mem::size_of, num::NonZeroUsize};

use super::{Analyzer, LogicalBuildGraph, LogicalClosure, LogicalClosureBody, TypedExprKind};
use crate::{
	datastore::Datastore,
	gql::{ExecutionControl, QueryExecutionOptions, execution::RootRead},
	gql_status::{Condition, TextSpan},
	syn::{
		BuildGraphSource,
		ClosureBody,
		DefinitionMode,
		Expr,
		ExprKind,
		LetValue,
		Statement,
		StatementKind,
	},
	test_assertions::{AssertOption as _, AssertResult as _},
	values::Object,
};

/// Builds an SDK binding without text from which a shell estimate could be
/// derived.
fn sdk_build() -> Statement {
	{
		let split_field_0 = StatementKind::Let({
			let split_field_0 = {
				let split_field_0 = "built".into();
				let split_field_1 = false;
				let split_field_2 = TextSpan::new(0, 0);
				crate::syn::split_internal::new_identifier(
					split_field_0,
					split_field_1,
					split_field_2,
				)
			};
			let split_field_1 = None;
			let split_field_2 = LetValue::BuildGraph(Box::new({
				let split_field_0 = None;
				let split_field_1 = DefinitionMode::Normal;
				let split_field_2 = None;
				let split_field_3 = None;
				let split_field_4 = BuildGraphSource::Expression({
					let split_field_0 = ExprKind::Array(Vec::new());
					let split_field_1 = TextSpan::new(0, 0);
					crate::syn::split_internal::new_expr(split_field_0, split_field_1)
				});
				let split_field_5 = None;
				let split_field_6 = None;
				let split_field_7 = None;
				let split_field_8 = None;
				let split_field_9 = None;
				let split_field_10 = None;
				crate::syn::split_internal::new_build_graph_statement(
					split_field_0,
					split_field_1,
					split_field_2,
					split_field_3,
					split_field_4,
					split_field_5,
					split_field_6,
					split_field_7,
					split_field_8,
					split_field_9,
					split_field_10,
				)
			}));
			crate::syn::split_internal::new_let_statement(
				split_field_0,
				split_field_1,
				split_field_2,
			)
		});
		let split_field_1 = TextSpan::new(0, 0);
		let split_field_2 = None;
		crate::syn::split_internal::new_statement(split_field_0, split_field_1, split_field_2)
	}
}

/// Wraps one expression in the single-RETURN block simplification path.
fn sdk_return(expression: Expr) -> Statement {
	{
		let split_field_0 = StatementKind::Return({
			let split_field_0 = expression;
			let split_field_1 = None;
			crate::syn::split_internal::new_return_statement(split_field_0, split_field_1)
		});
		let split_field_1 = TextSpan::new(0, 0);
		let split_field_2 = None;
		crate::syn::split_internal::new_statement(split_field_0, split_field_1, split_field_2)
	}
}

/// Reserves a caller-visible barrier cap without changing any other policy.
fn options(bytes: usize) -> QueryExecutionOptions {
	QueryExecutionOptions {
		barrier_memory_bytes: NonZeroUsize::new(bytes).assert_some(),
		..QueryExecutionOptions::default()
	}
}

/// Empty SDK spans still admit the new pointee and reject before committing its
/// ledger.
#[test]
fn empty_sdk_build_admits_exact_shell_and_leaves_failed_ledger_unchanged() {
	let store = Datastore::in_memory();
	let root = store.inner.current_root();
	let variables = Object::new();
	let shell = size_of::<LogicalBuildGraph>();
	for cap in [shell - 1, shell] {
		let control = ExecutionControl::new(&options(cap), None);
		let read = RootRead::new(&root, &control, &|_| {}, &|_| Ok(()), std::env::temp_dir());
		let mut analyzer = Analyzer::new("", &read, &variables, &BTreeSet::new());
		let result = analyzer.statement(&sdk_build(), 0);
		if cap < shell {
			assert_eq!(result.assert_err().status().condition(), Condition::ProgramLimitExceeded);
			assert_eq!(analyzer.layout_shell_bytes.get(), 0);
			assert_eq!(control.barrier_memory_available(), cap);
		} else {
			result.assert_ok();
			assert_eq!(analyzer.layout_shell_bytes.get(), shell);
			assert_eq!(control.barrier_memory_available(), 0);
		}
	}
}

/// Invalid BUILD semantics retain precedence over the later fresh-shell
/// admission.
#[test]
fn sdk_build_semantic_error_precedes_shell_limit() {
	let store = Datastore::in_memory();
	let root = store.inner.current_root();
	let control = ExecutionControl::new(&options(1), None);
	let read = RootRead::new(&root, &control, &|_| {}, &|_| Ok(()), std::env::temp_dir());
	let variables = Object::new();
	let mut analyzer = Analyzer::new("", &read, &variables, &BTreeSet::new());
	let mut syntax = sdk_build();
	if let StatementKind::Let(binding) = &mut syntax.kind
		&& let LetValue::BuildGraph(build) = &mut binding.value
	{
		build.mode = DefinitionMode::Overwrite;
	}
	let error = analyzer.statement(&syntax, 0).assert_err();
	assert!(error.to_string().contains("Anonymous BUILD GRAPH"));
	assert_eq!(analyzer.layout_shell_bytes.get(), 0);
	assert_eq!(control.barrier_memory_available(), 1);
}

/// Child constructors share admission while each retained subtree owns its full
/// copy delta.
#[test]
fn nested_single_return_closure_merges_shells_once_without_source_text() {
	let store = Datastore::in_memory();
	let root = store.inner.current_root();
	let variables = Object::new();
	let inner = {
		let split_field_0 = Vec::new();
		let split_field_1 = None;
		let split_field_2 = ClosureBody::Block({
			let split_field_0 = vec![
				sdk_build(),
				sdk_return(crate::syn::split_internal::new_expr(
					ExprKind::Parameter(crate::syn::split_internal::new_identifier(
						"built".into(),
						false,
						TextSpan::new(0, 0),
					)),
					TextSpan::new(0, 0),
				)),
			];
			let split_field_1 = TextSpan::new(0, 0);
			crate::syn::split_internal::new_block(split_field_0, split_field_1)
		});
		crate::syn::split_internal::new_closure_expr(split_field_0, split_field_1, split_field_2)
	};
	let outer = {
		let split_field_0 = Vec::new();
		let split_field_1 = None;
		let split_field_2 = ClosureBody::Block({
			let split_field_0 = vec![sdk_return(crate::syn::split_internal::new_expr(
				ExprKind::Closure(inner),
				TextSpan::new(0, 0),
			))];
			let split_field_1 = TextSpan::new(0, 0);
			crate::syn::split_internal::new_block(split_field_0, split_field_1)
		});
		crate::syn::split_internal::new_closure_expr(split_field_0, split_field_1, split_field_2)
	};
	let inner_delta = size_of::<LogicalBuildGraph>() + size_of::<usize>();
	let total = inner_delta + size_of::<usize>();
	let control = ExecutionControl::new(&options(total), None);
	let read = RootRead::new(&root, &control, &|_| {}, &|_| Ok(()), std::env::temp_dir());
	let analyzer = Analyzer::new("", &read, &variables, &BTreeSet::new());
	let closure = analyzer.closure(&outer, TextSpan::new(0, 0)).assert_ok();
	assert_eq!(closure.boxed_layout_bytes, total);
	assert_eq!(closure.retained_plan_bytes, size_of::<LogicalClosure>() + inner_delta);
	assert_eq!(analyzer.layout_shell_bytes.get(), total);
	assert_eq!(control.barrier_memory_available(), 0);
	let body = match &closure.body {
		LogicalClosureBody::Expression(expression) => Some(expression),
		LogicalClosureBody::Block(_) => None,
	}
	.assert_some_with("single RETURN must retain expression-body lowering");
	let nested = match &body.kind {
		TypedExprKind::Closure(closure) => Some(closure),
		_ => None,
	}
	.assert_some_with("returned nested closure must retain its typed plan");
	assert_eq!(nested.boxed_layout_bytes, inner_delta);
	assert!(matches!(nested.body, LogicalClosureBody::Block(_)));
}

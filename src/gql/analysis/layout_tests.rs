//! Fixed-shell accounting is independent of source spans and nesting depth.

use std::{collections::BTreeSet, mem::size_of, num::NonZeroUsize};

use super::{Analyzer, LogicalBuildGraph, LogicalClosure, LogicalClosureBody, TypedExprKind};
use crate::{
	datastore::Datastore,
	gql::{ExecutionControl, QueryExecutionOptions, execution::RootRead},
	gql_status::{Condition, TextSpan},
	syn::{
		Block,
		BuildGraphSource,
		BuildGraphStatement,
		ClosureBody,
		ClosureExpr,
		DefinitionMode,
		Expr,
		ExprKind,
		Identifier,
		LetStatement,
		LetValue,
		ReturnStatement,
		Statement,
		StatementKind,
	},
	test_assertions::{AssertOption as _, AssertResult as _},
	values::Object,
};

/// Builds an SDK binding without text from which a shell estimate could be
/// derived.
fn sdk_build() -> Statement {
	Statement {
		kind: StatementKind::Let(LetStatement {
			name: Identifier {
				value: "built".into(),
				quoted: false,
				span: TextSpan::new(0, 0),
			},
			declared_type: None,
			value: LetValue::BuildGraph(Box::new(BuildGraphStatement {
				bridge: None,
				mode: DefinitionMode::Normal,
				contract: None,
				name: None,
				source: BuildGraphSource::Expression(Expr {
					kind: ExprKind::Array(Vec::new()),
					span: TextSpan::new(0, 0),
				}),
				root: None,
				nodes: None,
				edges: None,
				limit_nodes: None,
				limit_edges: None,
				timeout: None,
			})),
		}),
		span: TextSpan::new(0, 0),
		timeout: None,
	}
}

/// Wraps one expression in the single-RETURN block simplification path.
fn sdk_return(expression: Expr) -> Statement {
	Statement {
		kind: StatementKind::Return(ReturnStatement {
			value: expression,
			alias: None,
		}),
		span: TextSpan::new(0, 0),
		timeout: None,
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
	let inner = ClosureExpr {
		parameters: Vec::new(),
		result_type: None,
		body: ClosureBody::Block(Block {
			statements: vec![
				sdk_build(),
				sdk_return(Expr {
					kind: ExprKind::Parameter(Identifier {
						value: "built".into(),
						quoted: false,
						span: TextSpan::new(0, 0),
					}),
					span: TextSpan::new(0, 0),
				}),
			],
			span: TextSpan::new(0, 0),
		}),
	};
	let outer = ClosureExpr {
		parameters: Vec::new(),
		result_type: None,
		body: ClosureBody::Block(Block {
			statements: vec![sdk_return(Expr {
				kind: ExprKind::Closure(inner),
				span: TextSpan::new(0, 0),
			})],
			span: TextSpan::new(0, 0),
		}),
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

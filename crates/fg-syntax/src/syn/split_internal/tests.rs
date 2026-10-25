//! Characterization checks for the shared immediate-child traversal boundary.

use super::{Expr, ExprKind, Identifier, KnnSearchArgument, TextSpan, any_value_child};

/// Creates one named leaf without invoking the parser or analyzer.
fn name(value: &str) -> Expr {
	Expr {
		kind: ExprKind::Name(Identifier {
			value: value.into(),
			quoted: false,
			span: TextSpan::new(0, 0),
		}),
		span: TextSpan::new(0, 0),
	}
}

/// Observes direct names and permits a selected leaf to short-circuit.
fn visited(expression: &Expr, stop: &str) -> Vec<String> {
	let mut result = Vec::new();
	any_value_child(expression, |child| {
		if let ExprKind::Name(identifier) = &child.kind {
			result.push(identifier.value.to_string());
			identifier.value.as_ref() == stop
		} else {
			false
		}
	});
	result
}

/// KNN children preserve admission order and stop before later operands.
#[test]
fn knn_order_and_short_circuit_are_preserved() {
	let expression = Expr {
		kind: ExprKind::Knn {
			left: Box::new(name("left")),
			k: Box::new(name("k")),
			search: Some(KnnSearchArgument::Ef(Box::new(name("search")))),
			right: Box::new(name("right")),
		},
		span: TextSpan::new(0, 0),
	};
	assert_eq!(visited(&expression, ""), ["left", "k", "search", "right"]);
	assert_eq!(visited(&expression, "search"), ["left", "k", "search"]);
}

/// CASE preserves operand/branch/else order; calls do not visit their callee.
#[test]
fn case_order_and_call_boundary_are_preserved() {
	let expression = Expr {
		kind: ExprKind::Case {
			operand: Some(Box::new(name("operand"))),
			branches: vec![(name("condition"), name("value"))],
			else_value: Some(Box::new(name("else"))),
		},
		span: TextSpan::new(0, 0),
	};
	assert_eq!(visited(&expression, ""), ["operand", "condition", "value", "else"]);
	assert_eq!(visited(&expression, "condition"), ["operand", "condition"]);
	let call = Expr {
		kind: ExprKind::Call {
			callee: Box::new(name("callee")),
			type_argument: None,
			distinct: false,
			argument_names: Vec::new(),
			arguments: vec![name("argument")],
		},
		span: TextSpan::new(0, 0),
	};
	assert_eq!(visited(&call, ""), ["argument"]);
}

/// Nested lexical scopes remain opaque even when they contain value children.
#[test]
fn nested_scopes_are_not_visited() {
	let span = TextSpan::new(0, 0);
	let closure = Expr {
		kind: ExprKind::Closure(super::ClosureExpr {
			parameters: Vec::new(),
			result_type: None,
			body: super::ClosureBody::Expression(Box::new(name("captured"))),
		}),
		span,
	};
	let query = Expr {
		kind: ExprKind::Subquery(Box::new(super::SelectStatement::independent_union(
			Vec::new(),
			0,
		))),
		span,
	};
	for expression in [closure, query] {
		let mut calls = 0;
		assert!(!any_value_child(&expression, |_| {
			calls += 1;
			true
		}));
		assert_eq!(calls, 0);
	}
}

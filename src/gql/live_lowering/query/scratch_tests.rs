//! Resource regressions for direct root streaming and retained results.

use super::*;
use crate::{
	datastore::{ClassField, ClassSchema, Datastore},
	graph::AtomKind,
	live::{LiveExpr, LiveProjection},
	values::{PropertyMap, Type},
};

/// Builds a fixed scalar shape with four matching rows and optional long keys.
fn streaming_fixture(rows: usize, key_bytes: usize) -> FerrosGrynnResult<(Datastore, ClassId)> {
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class = setup.define_class(ClassSchema::new("item", AtomKind::Node, [
		ClassField::new("keep", Type::Bool),
		ClassField::new("score", Type::Int),
	])?)?;
	let keep = setup.intern_property("keep")?;
	let score = setup.intern_property("score")?;
	for ordinal in 0..rows {
		setup.create_atom(
			crate::values::RecordId::new(
				"item",
				crate::values::RecordIdPart::String(
					format!("r{ordinal}{}", "x".repeat(key_bytes)).into(),
				),
			)?,
			PropertyMap::from_iter([
				(keep, Value::from(ordinal < 4)),
				(score, Value::from(ordinal)),
			]),
		)?;
	}
	setup.commit()?;
	Ok((store, class))
}

/// Discarded WHERE rows must release their transient copy reservations.
#[test]
fn live_streaming_discarded_rows_charge_peak_scratch() -> FerrosGrynnResult<()> {
	let (store, class) = streaming_fixture(4096, 0)?;
	let query = LiveQuery::new(LiveSource::Class(class))
		.with_filter(LiveExpr::field("keep")?)
		.with_projection(LiveProjection::Value(LiveExpr::field("score")?));
	let result = evaluate_query(
		&store.inner.current_root(),
		&query,
		&|_id| Err(FerrosGrynnError::invalid_argument("unexpected binding")),
		&LiveOptions::default().with_budgets(1 << 20, 1_000_000)?,
		std::env::temp_dir(),
		&|_| {},
	)?;
	assert_eq!(result.rows.len(), 4);
	for ordinal in 0..4 {
		let identity = LiveIdentity::Record(format!("item:r{ordinal}").parse()?);
		assert_eq!(result.rows.get(&identity), Some(&Value::from(ordinal)));
	}
	let last = store
		.inner
		.current_root()
		.atom_by_record(&"item:r4095".parse()?)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture record missing"))?
		.id();
	assert!(result.reads.contains(&ConflictKey::BaseClassRange(class)));
	assert!(!result.reads.contains(&ConflictKey::BaseAtom(last)));
	Ok(())
}

/// Resource rejection never returns a successful partial result map.
fn assert_resource(result: FerrosGrynnResult<QueryEvaluation>) -> FerrosGrynnResult<()> {
	let Err(error) = result else {
		return Err(FerrosGrynnError::invalid_argument("resource fixture unexpectedly succeeded"));
	};
	let mut cause = Some(&error);
	while let Some(current) = cause {
		if matches!(
			current.status().status(),
			crate::gql_status::Status::TraversalResourceLimitExceeded { .. }
		) {
			return Ok(());
		}
		cause = current.cause();
	}
	Err(error)
}

/// Retained projected values remain charged after each source row is dropped.
#[test]
fn live_streaming_retained_outputs_remain_bounded() -> FerrosGrynnResult<()> {
	let (store, class) = streaming_fixture(512, 0)?;
	let query = LiveQuery::new(LiveSource::Class(class))
		.with_projection(LiveProjection::Value(LiveExpr::Literal("x".repeat(16 << 10).into())));
	assert_resource(evaluate_query(
		&store.inner.current_root(),
		&query,
		&|_id| Err(FerrosGrynnError::invalid_argument("unexpected binding")),
		&LiveOptions::default().with_budgets(8 << 20, 1_000_000)?,
		std::env::temp_dir(),
		&|_| {},
	))
}

/// Tiny projections cannot hide large retained record identities.
#[test]
fn live_streaming_retained_identity_remains_bounded() -> FerrosGrynnResult<()> {
	let (store, class) = streaming_fixture(512, 4096)?;
	let query = LiveQuery::new(LiveSource::Class(class))
		.with_projection(LiveProjection::Value(LiveExpr::Literal(0.into())));
	assert_resource(evaluate_query(
		&store.inner.current_root(),
		&query,
		&|_id| Err(FerrosGrynnError::invalid_argument("unexpected binding")),
		&LiveOptions::default().with_budgets(4 << 20, 1_000_000)?,
		std::env::temp_dir(),
		&|_| {},
	))
}

/// One oversized input still fails before constructing its public row copy.
#[test]
fn live_streaming_oversized_row_still_rejects() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class =
		setup.define_class(ClassSchema::new("wide", AtomKind::Node, [ClassField::new(
			"payload",
			Type::String,
		)])?)?;
	let payload = setup.intern_property("payload")?;
	setup.create_atom(
		"wide:one".parse()?,
		PropertyMap::from_iter([(payload, "x".repeat(256 << 10).into())]),
	)?;
	setup.commit()?;
	let query = LiveQuery::new(LiveSource::Class(class))
		.with_projection(LiveProjection::Value(LiveExpr::Literal(0.into())));
	assert_resource(evaluate_query(
		&store.inner.current_root(),
		&query,
		&|_id| Err(FerrosGrynnError::invalid_argument("unexpected binding")),
		&LiveOptions::default().with_budgets(320 << 10, 1_000_000)?,
		std::env::temp_dir(),
		&|_| {},
	))
}

/// Nested collectors keep payloads admitted while their outer row is alive.
#[test]
fn live_streaming_nested_outputs_remain_bounded() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class =
		setup.define_class(ClassSchema::new("wide", AtomKind::Node, [ClassField::new(
			"payload",
			Type::String,
		)])?)?;
	let payload = setup.intern_property("payload")?;
	setup.create_atom(
		"wide:one".parse()?,
		PropertyMap::from_iter([(payload, "x".repeat(256 << 10).into())]),
	)?;
	let outer = setup.define_class(ClassSchema::new("marker", AtomKind::Node, [])?)?;
	setup.create_atom("marker:one".parse()?, PropertyMap::new())?;
	setup.commit()?;
	let nested = LiveQuery::new(LiveSource::Class(class)).with_projection(LiveProjection::Value(
		LiveExpr::Array(vec![LiveExpr::field("payload")?; 8]),
	));
	let query = LiveQuery::new(LiveSource::Class(outer))
		.with_projection(LiveProjection::Value(LiveExpr::Select(Box::new(nested))));
	assert_resource(evaluate_query(
		&store.inner.current_root(),
		&query,
		&|_id| Err(FerrosGrynnError::invalid_argument("unexpected binding")),
		&LiveOptions::default().with_budgets(8 << 20, 1_000_000)?,
		std::env::temp_dir(),
		&|_| {},
	))
}

/// Nested scopes restore reservations; failed reservations do not consume
/// bytes.
#[test]
fn live_streaming_scope_restore_and_failure_cleanup() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let root = store.inner.current_root();
	let observe = |_| {};
	let work = |_| Ok(());
	let control =
		crate::gql::ExecutionControl::new(&crate::gql::QueryExecutionOptions::default(), None);
	let read = RootRead::new(&root, &control, &observe, &work, std::env::temp_dir());
	let fields = read.fields();
	let maximum = control.barrier_memory_limit();
	let quarter = maximum / 4;
	fields.admit_retained(quarter)?;
	{
		let _outer = fields
			.transient_scope()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("scope missing"))?;
		fields.admit(2 * quarter)?;
		{
			let _inner = fields
				.transient_scope()
				.ok_or_else(|| FerrosGrynnError::invalid_argument("scope missing"))?;
			fields.admit(quarter)?;
			assert!(fields.admit(1).is_err());
		}
		fields.admit(quarter)?;
	}
	fields.admit_retained(maximum - quarter)?;
	assert!(fields.admit_retained(1).is_err());
	for cancel in [false, true] {
		let cancellation = crate::gql::QueryCancellation::default();
		let options = crate::gql::QueryExecutionOptions {
			cancellation: Some(cancellation.clone()),
			..Default::default()
		};
		let control = crate::gql::ExecutionControl::new(&options, None);
		let read = RootRead::new(&root, &control, &observe, &work, std::env::temp_dir());
		let fields = read.fields();
		let outcome = (|| -> FerrosGrynnResult<()> {
			let _scope = fields
				.transient_scope()
				.ok_or_else(|| FerrosGrynnError::invalid_argument("scope missing"))?;
			fields.admit(control.barrier_memory_limit())?;
			if cancel {
				cancellation.cancel();
				control.check()?;
			}
			Err(FerrosGrynnError::invalid_argument("failed callback"))
		})();
		assert!(outcome.is_err());
		fields.admit_retained(control.barrier_memory_limit())?;
	}
	Ok(())
}

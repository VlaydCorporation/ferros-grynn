//! Contract tests exercise the ordinary public transaction path.

use super::*;
use crate::{
	datastore::{ClassSchema, Datastore},
	gql_status::FerrosGrynnResult,
	graph::AtomKind,
	values::{PropertyMap, RecordId},
};

/// Builds the fixture fixture used by nearby scenarios.
fn fixture() -> FerrosGrynnResult<(Datastore, BridgeDatabaseRef, RecordId, RecordId)> {
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
	let a: RecordId = "person:a".parse()?;
	let b: RecordId = "person:b".parse()?;
	tx.create_atom(a.clone(), PropertyMap::new())?;
	tx.create_atom(b.clone(), PropertyMap::new())?;
	let database = tx.register_bridge_database(
		BridgeDatabaseOptions::new("sdb", "fake", "test", "test")?
			.map_class("person", "profile")?,
	)?;
	tx.commit()?;
	Ok((store, database, a, b))
}

/// Verifies bridge registry is bidirectional and idempotent.
#[test]
fn bridge_registry_is_bidirectional_and_idempotent() {
	use crate::test_assertions::AssertResult as _;

	let (store, database, a, b) = fixture().assert_ok();
	let external = BridgeEntityRef {
		database,
		record: "profile:a".parse().assert_ok(),
	};
	let subject = BridgeMappingSubject::Atom(a);
	let mut tx = store.begin();
	let reference = tx.register_bridge_mapping(subject.clone(), external.clone()).assert_ok();
	assert_eq!(
		tx.register_bridge_mapping(subject.clone(), external.clone()).assert_ok(),
		reference
	);
	assert!(tx.register_bridge_mapping(BridgeMappingSubject::Atom(b), external.clone()).is_err());
	tx.commit().assert_ok();
	let mut tx = store.begin();
	assert_eq!(
		tx.bridge_mapping_local(database, &subject).assert_ok().map(|row| row.reference()),
		Some(reference)
	);
	assert_eq!(
		tx.bridge_mapping_external(&external, BridgeMappingRole::Atom)
			.assert_ok()
			.map(|row| row.reference()),
		Some(reference)
	);
	assert_eq!(
		tx.bridge_mapping(reference).assert_ok().map(|row| row.state()),
		Some(BridgeMappingState::Ready)
	);
}

/// Verifies bridge registry preserves snapshot and abort.
#[test]
fn bridge_registry_preserves_snapshot_and_abort() {
	use crate::test_assertions::AssertResult as _;

	let (store, database, a, _) = fixture().assert_ok();
	let subject = BridgeMappingSubject::Atom(a);
	let mut old = store.begin();
	assert!(old.bridge_mapping_local(database, &subject).assert_ok().is_none());
	let mut tx = store.begin();
	let reference = tx
		.register_bridge_mapping(subject.clone(), BridgeEntityRef {
			database,
			record: "profile:a".parse().assert_ok(),
		})
		.assert_ok();
	drop(tx);
	assert!(store.begin().bridge_mapping(reference).assert_ok().is_none());
	let mut tx = store.begin();
	tx.register_bridge_mapping(subject.clone(), BridgeEntityRef {
		database,
		record: "profile:a".parse().assert_ok(),
	})
	.assert_ok();
	tx.commit().assert_ok();
	assert!(old.bridge_mapping_local(database, &subject).assert_ok().is_none());
	assert!(store.begin().bridge_mapping_local(database, &subject).assert_ok().is_some());
}

/// Verifies bridge registry concurrent unique reservations conflict.
#[test]
fn bridge_registry_concurrent_unique_reservations_conflict() {
	use crate::test_assertions::AssertResult as _;

	let (store, database, a, b) = fixture().assert_ok();
	let mut one = store.begin();
	let mut two = store.begin();
	let external = BridgeEntityRef {
		database,
		record: "profile:shared".parse().assert_ok(),
	};
	one.register_bridge_mapping(BridgeMappingSubject::Atom(a), external.clone()).assert_ok();
	two.register_bridge_mapping(BridgeMappingSubject::Atom(b), external).assert_ok();
	one.commit().assert_ok();
	assert!(two.commit().is_err());
}

/// Verifies bridge registry tombstones deleted subjects and rejects foreign
/// refs.
#[test]
fn bridge_registry_tombstones_deleted_subjects_and_rejects_foreign_refs() {
	use crate::test_assertions::AssertResult as _;

	let (store, database, a, _) = fixture().assert_ok();
	let mut tx = store.begin();
	let subject = BridgeMappingSubject::Atom(a.clone());
	let external = BridgeEntityRef {
		database,
		record: "profile:a".parse().assert_ok(),
	};
	let reference = tx.register_bridge_mapping(subject.clone(), external.clone()).assert_ok();
	tx.commit().assert_ok();
	let mut tx = store.begin();
	tx.delete_atom(&a).assert_ok();
	tx.commit().assert_ok();
	let mut tx = store.begin();
	assert_eq!(
		tx.bridge_mapping(reference).assert_ok().map(|row| row.state()),
		Some(BridgeMappingState::Tombstoned)
	);
	assert!(tx.bridge_mapping_local(database, &subject).assert_ok().is_none());
	assert!(tx.bridge_mapping_external(&external, BridgeMappingRole::Atom).assert_ok().is_none());
	assert!(Datastore::in_memory().begin().bridge_mapping(reference).is_err());
	assert_eq!(BridgeMappingRef::try_from(Value::from(reference)).assert_ok(), reference);
}

/// Verifies bridge registry rejects unrecoverable external names.
#[test]
fn bridge_registry_rejects_unrecoverable_external_names() {
	use crate::test_assertions::AssertResult as _;

	let (store, database, record, _) = fixture().assert_ok();
	for class in ["x".repeat(4097), "profile\0bad".to_owned()] {
		let external = RecordId::from_value(class, Value::from("a")).assert_ok();
		let mut tx = store.begin();
		assert!(
			tx.register_bridge_mapping(
				BridgeMappingSubject::Atom(record.clone()),
				BridgeEntityRef {
					database,
					record: external
				}
			)
			.is_err()
		);
		tx.commit().assert_ok();
	}
	let oversized =
		RecordId::from_value("profile", Value::Array(vec![Value::from("x".repeat(16_384))].into()))
			.assert_ok();
	assert!(
		store
			.begin()
			.register_bridge_mapping(BridgeMappingSubject::Atom(record.clone()), BridgeEntityRef {
				database,
				record: oversized
			})
			.is_err()
	);
	assert!(
		store
			.begin()
			.bridge_mapping_local(database, &BridgeMappingSubject::Atom(record))
			.assert_ok()
			.is_none()
	);
}

/// Verifies bridge sources reject shapes and memory before receiving writes.
#[test]
fn bridge_sources_reject_shapes_and_memory_before_receiving_writes() {
	use crate::test_assertions::AssertResult as _;

	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.map_err(|error| {
			crate::gql_status::FerrosGrynnError::bridge_operation_error("test runtime failed")
				.with_technical_source(error)
		})
		.assert_ok();
	let test_result: FerrosGrynnResult<()> = runtime.block_on(async {
		let (store, _, _, _) = fixture()?;
		assert!(
			store
				.query("CREATE person:bad")
				.use_bridge(
					ConstantBridge(Value::Array(Vec::<Value>::new().into())),
					BridgeData::ExternalQuery(
						"source",
						Some(std::collections::HashMap::from([("field", "target")]))
					)
				)
				.execute()
				.await
				.is_err()
		);
		assert!(
			store
				.query("CREATE person:bad")
				.use_bridge(
					ConstantBridge(Value::Object(Object::new())),
					BridgeData::ExternalQuery(
						"source",
						Some(std::collections::HashMap::from([("field", "target")]))
					)
				)
				.execute()
				.await
				.is_err()
		);
		let options = crate::gql::QueryExecutionOptions {
			barrier_memory_bytes: std::num::NonZeroUsize::new(512).ok_or_else(|| {
				crate::gql_status::FerrosGrynnError::invalid_argument("test budget")
			})?,
			..Default::default()
		};
		assert!(
			store
				.query("CREATE person:bad")
				.options(options)
				.use_bridge(
					ConstantBridge(Value::from("x".repeat(4096))),
					BridgeData::ExternalQuery("source", None)
				)
				.execute()
				.await
				.is_err()
		);
		assert!(store.begin().atom(&"person:bad".parse()?)?.is_none());
		Ok(())
	});
	test_result.assert_ok();
}

/// Verifies bridge registry local reopen preserves both directions.
#[test]
fn bridge_registry_local_reopen_preserves_both_directions() {
	use crate::{
		datastore::{CheckpointMode, LocalOptions},
		test_assertions::AssertResult as _,
	};
	let directory =
		std::env::temp_dir().join(format!("fgb30-{:016x}", uuid::Uuid::now_v7().as_u128() as u64));
	let options_defaults = LocalOptions::default();
	let options = LocalOptions {
		buffer_pool_pages: 64,
		..options_defaults
	};
	let store = Datastore::create_local(&directory, options).assert_ok();
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	let record: RecordId = "person:a".parse().assert_ok();
	tx.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	let database = tx
		.register_bridge_database(
			BridgeDatabaseOptions::new("sdb", "fake", "test", "test")
				.assert_ok()
				.map_class("person", "profile")
				.assert_ok(),
		)
		.assert_ok();
	let subject = BridgeMappingSubject::Atom(record.clone());
	let external = BridgeEntityRef {
		database,
		record: "profile:a".parse().assert_ok(),
	};
	let reference = tx.register_bridge_mapping(subject.clone(), external.clone()).assert_ok();
	tx.commit().assert_ok();
	store.checkpoint(CheckpointMode::Passive).assert_ok();
	drop(store);
	let store = Datastore::open_local(&directory, options).assert_ok();
	assert_eq!(
		store.bridge_database("sdb").assert_ok().map(|database| database.reference()),
		Some(database)
	);
	assert_eq!(
		store
			.begin()
			.bridge_mapping_local(database, &subject)
			.assert_ok()
			.map(|row| row.reference()),
		Some(reference)
	);
	assert_eq!(
		store
			.begin()
			.bridge_mapping_external(&external, BridgeMappingRole::Atom)
			.assert_ok()
			.map(|row| row.reference()),
		Some(reference)
	);
	let mut tx = store.begin();
	tx.delete_atom(&record).assert_ok();
	tx.commit().assert_ok();
	drop(store);
	let store = Datastore::open_local(&directory, options).assert_ok();
	assert_eq!(
		store.begin().bridge_mapping(reference).assert_ok().map(|row| row.state()),
		Some(BridgeMappingState::Tombstoned)
	);
	drop(store);
	std::fs::remove_dir_all(&directory)
		.map_err(|error| {
			crate::gql_status::FerrosGrynnError::bridge_operation_error(
				"test directory cleanup failed",
			)
			.with_technical_source(error)
		})
		.assert_ok();
}

/// Bridge test double returning a fixed result for capability checks.
struct ConstantBridge(
	/// Fixed value returned by the bridge test double.
	crate::values::Value,
);
impl Bridge for ConstantBridge {
	/// Returns execute for `ConstantBridge`.
	fn execute(&self, _operation: BridgeOperation, context: BridgeContext) -> BridgeFuture<'_> {
		Box::pin(async move {
			context.check()?;
			Ok(self.0.clone())
		})
	}
}

/// Verifies bridge registry roles databases occurrences and ssi.
#[test]
fn bridge_registry_roles_databases_occurrences_and_ssi() {
	use crate::{
		graph::{EdgeTopology, GraphKind},
		test_assertions::AssertResult as _,
	};
	let (store, database, a, b) = fixture().assert_ok();
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let edge: RecordId = "link:r".parse().assert_ok();
	tx.create_atom(edge.clone(), PropertyMap::new()).assert_ok();
	let second = tx
		.register_bridge_database(
			BridgeDatabaseOptions::new("other", "fake", "test", "test").assert_ok(),
		)
		.assert_ok();
	let g = tx.define_graph("g", GraphKind::Graph).assert_ok();
	let h = tx.define_graph("h", GraphKind::Graph).assert_ok();
	for graph in [g.id(), h.id()] {
		let source = tx.attach_node(graph, &a, None, None).assert_ok();
		let target = tx.attach_node(graph, &b, None, None).assert_ok();
		tx.attach_edge(graph, &edge, None, EdgeTopology::directed([source], [target]), None, None)
			.assert_ok();
	}
	let entity = BridgeEntityRef {
		database,
		record: "foreign:shared".parse().assert_ok(),
	};
	let atom = tx
		.register_bridge_mapping(BridgeMappingSubject::Atom(edge.clone()), entity.clone())
		.assert_ok();
	let relation = tx
		.register_bridge_mapping(
			BridgeMappingSubject::Relation {
				graph: g.id(),
				edge: edge.clone(),
			},
			entity.clone(),
		)
		.assert_ok();
	let occurrence = tx
		.register_bridge_mapping(
			BridgeMappingSubject::Relation {
				graph: h.id(),
				edge: edge.clone(),
			},
			BridgeEntityRef {
				database,
				record: "foreign:h".parse().assert_ok(),
			},
		)
		.assert_ok();
	let other = tx
		.register_bridge_mapping(BridgeMappingSubject::Atom(edge.clone()), BridgeEntityRef {
			database: second,
			record: entity.record,
		})
		.assert_ok();
	assert_ne!(atom, relation);
	assert_ne!(atom, other);
	tx.commit().assert_ok();
	let mut tx = store.begin();
	tx.remove_graph("g").assert_ok();
	tx.commit().assert_ok();
	let mut tx = store.begin();
	assert_eq!(
		tx.bridge_mapping(relation).assert_ok().map(|row| row.state()),
		Some(BridgeMappingState::Tombstoned)
	);
	for reference in [atom, other, occurrence] {
		assert_eq!(
			tx.bridge_mapping(reference).assert_ok().map(|row| row.state()),
			Some(BridgeMappingState::Ready)
		);
	}
	let mut reader = store.begin_with(crate::datastore::TransactionOptions::new(
		crate::datastore::IsolationLevel::Serializable,
		crate::datastore::TransactionAccess::ReadWrite,
	));
	let subject = BridgeMappingSubject::Atom(a.clone());
	assert!(reader.bridge_mapping_local(database, &subject).assert_ok().is_none());
	let mut writer = store.begin();
	writer
		.register_bridge_mapping(subject, BridgeEntityRef {
			database,
			record: "profile:a".parse().assert_ok(),
		})
		.assert_ok();
	writer.commit().assert_ok();
	for result in reader.execute("UPDATE person:b SET probe = 1", &Object::new()).assert_ok() {
		result.result.assert_ok();
	}
	assert!(reader.commit().is_err());
}

/// Verifies bridge registry pending overflow recovery and import tombstone.
#[test]
fn bridge_registry_pending_overflow_recovery_and_import_tombstone() {
	use crate::{
		datastore::LocalOptions,
		graph::{EdgeTopology, GraphKind},
		test_assertions::AssertResult as _,
	};
	let directory =
		std::env::temp_dir().join(format!("fgb30p-{:016x}", uuid::Uuid::now_v7().as_u128() as u64));
	let options_defaults = LocalOptions::default();
	let mut options = LocalOptions {
		buffer_pool_pages: 64,
		..options_defaults
	};
	options.maintenance.max_memory_bytes = std::num::NonZeroUsize::new(72 * 1024 * 1024)
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument("test maintenance budget")
		})
		.assert_ok();
	let store = Datastore::create_local(&directory, options).assert_ok();
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	tx.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let g = tx.define_graph("g", GraphKind::Graph).assert_ok();
	let a: RecordId = "person:a".parse().assert_ok();
	let b: RecordId = "person:b".parse().assert_ok();
	let edge: RecordId = "link:r".parse().assert_ok();
	let spare: RecordId = "link:spare".parse().assert_ok();
	for record in [&a, &b, &edge, &spare] {
		tx.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let source = tx.attach_node(g.id(), &a, None, None).assert_ok();
	let target = tx.attach_node(g.id(), &b, None, None).assert_ok();
	tx.attach_edge(g.id(), &edge, None, EdgeTopology::directed([source], [target]), None, None)
		.assert_ok();
	tx.attach_edge(g.id(), &spare, None, EdgeTopology::directed([source], [target]), None, None)
		.assert_ok();
	let database = tx
		.register_bridge_database(
			BridgeDatabaseOptions::new("sdb", "fake", "ns", "db")
				.assert_ok()
				.map_graph(g.id(), "relations")
				.assert_ok(),
		)
		.assert_ok();
	let external = BridgeEntityRef {
		database,
		record: "relations:r".parse().assert_ok(),
	};
	let subject = BridgeMappingSubject::Relation {
		graph: g.id(),
		edge,
	};
	let intent = BridgeMirrorRequest {
		relation: external.record.clone(),
		source: "profile:a".parse().assert_ok(),
		target: "profile:b".parse().assert_ok(),
		properties: Object::from_iter([("payload", Value::from("x".repeat(3 * 1024 * 1024)))]),
	};
	let reference =
		tx.reserve_bridge_mapping(subject.clone(), external.clone(), intent).assert_ok();
	tx.commit().assert_ok();
	drop(store);
	let store = Datastore::open_local(&directory, options).assert_ok();
	let row = store
		.begin()
		.bridge_mapping(reference)
		.assert_ok()
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::bridge_mapping_error("test mapping absent")
		})
		.assert_ok();
	assert_eq!(row.state(), BridgeMappingState::Pending);
	assert_eq!(
		row.mirror().and_then(|intent| intent.properties.get("payload")).and_then(|value| {
			if let Value::String(text) = value {
				Some(text.len())
			} else {
				None
			}
		}),
		Some(3 * 1024 * 1024)
	);
	let mut tx = store.begin();
	let mut intent = row
		.mirror()
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::bridge_mapping_error("test intent absent")
		})
		.assert_ok()
		.clone();
	intent.relation = "relations:spare".parse().assert_ok();
	let rejected = tx
		.reserve_bridge_mapping(
			BridgeMappingSubject::Relation {
				graph: g.id(),
				edge: spare,
			},
			BridgeEntityRef {
				database,
				record: intent.relation.clone(),
			},
			intent,
		)
		.assert_ok();
	assert!(
		tx.commit().is_err(),
		"publication must reject a registry that cannot reopen within the same budget"
	);
	assert!(store.begin().bridge_mapping(rejected).assert_ok().is_none());
	let mut tx = store.begin();
	tx.confirm_bridge_mapping(reference).assert_ok();
	tx.commit().assert_ok();
	drop(store);
	let store = Datastore::open_local(&directory, options).assert_ok();
	assert_eq!(
		store.begin().bridge_mapping(reference).assert_ok().map(|row| row.state()),
		Some(BridgeMappingState::Ready)
	);
	for result in store
		.execute(
			"BEGIN IMPORT; DELETE person:a CASCADE; END IMPORT WITH VALIDATION;",
			&Object::new(),
		)
		.assert_ok()
	{
		result.result.assert_ok();
	}
	let mut tx = store.begin();
	tx.define_parameter(
		crate::datastore::SchemaParameter::new("catalog_probe", Value::from(1_i64)).assert_ok(),
	)
	.assert_ok();
	tx.commit().assert_ok();
	drop(store);
	let store = Datastore::open_local(&directory, options).assert_ok();
	assert_eq!(
		store.begin().bridge_mapping(reference).assert_ok().map(|row| row.state()),
		Some(BridgeMappingState::Tombstoned)
	);
	assert!(store.begin().bridge_mapping_local(database, &subject).assert_ok().is_none());
	assert!(
		store
			.begin()
			.bridge_mapping_external(&external, BridgeMappingRole::Relation)
			.assert_ok()
			.is_none()
	);
	drop(store);
	std::fs::remove_dir_all(&directory)
		.map_err(|error| {
			crate::gql_status::FerrosGrynnError::bridge_operation_error("test cleanup failed")
				.with_technical_source(error)
		})
		.assert_ok();
}

/// Verifies bridge query sources preserve values and override bindings.
#[test]
fn bridge_query_sources_preserve_values_and_override_bindings() {
	use crate::test_assertions::AssertResult as _;

	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.map_err(|error| {
			crate::gql_status::FerrosGrynnError::bridge_operation_error("test runtime failed")
				.with_technical_source(error)
		})
		.assert_ok();
	let test_result: FerrosGrynnResult<()> = runtime.block_on(async {
		let store = Datastore::in_memory();
		let value = Value::Array(vec![Value::None, Value::Null, Value::from(7_i64)].into());
		let result = store
			.query("RETURN [$bridge, $ordinary, $first]")
			.bind(Object::from_iter([
				("bridge", Value::from(0_i64)),
				("first", Value::from(1_i64)),
			]))
			.bind(Object::from_iter([("ordinary", Value::from(2_i64))]))
			.use_bridge(
				ConstantBridge(value.clone()),
				BridgeData::ExternalQuery("native query", None),
			)
			.execute()
			.await?;
		assert_eq!(
			result
				.results
				.into_iter()
				.next()
				.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument(
					"test result missing"
				))?
				.result?
				.into_parts()
				.0,
			Value::Array(vec![value, Value::from(2_i64), Value::from(1_i64)].into())
		);
		let result = store
			.query("RETURN $target")
			.use_bridge(
				ConstantBridge(Value::Object(Object::from_iter([("field", Value::from(9_i64))]))),
				BridgeData::ExternalQuery(
					"native query",
					Some(std::collections::HashMap::from([("field", "target")])),
				),
			)
			.execute()
			.await?;
		assert_eq!(
			result
				.results
				.into_iter()
				.next()
				.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument(
					"test result missing"
				))?
				.result?
				.into_parts()
				.0,
			Value::from(9_i64)
		);
		assert!(
			store
				.query("RETURN 1")
				.use_bridge(ConstantBridge(Value::Null), BridgeData::ExternalQuery("native", None))
				.use_bridge(ConstantBridge(Value::Null), BridgeData::ExternalQuery("native", None))
				.execute()
				.await
				.is_err()
		);
		Ok(())
	});
	test_result.assert_ok();
}

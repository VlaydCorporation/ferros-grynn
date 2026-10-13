//! Contract tests exercise the ordinary public transaction path.

use super::*;
use crate::{
	datastore::{ClassSchema, Datastore},
	gql_status::FerrosGrynnResult,
	graph::AtomKind,
	values::{PropertyMap, RecordId},
};

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

#[test]
fn bridge_registry_is_bidirectional_and_idempotent() -> FerrosGrynnResult<()> {
	let (store, database, a, b) = fixture()?;
	let external = BridgeEntityRef {
		database,
		record: "profile:a".parse()?,
	};
	let subject = BridgeMappingSubject::Atom(a.clone());
	let mut tx = store.begin();
	let reference = tx.register_bridge_mapping(subject.clone(), external.clone())?;
	assert_eq!(tx.register_bridge_mapping(subject.clone(), external.clone())?, reference);
	assert!(tx.register_bridge_mapping(BridgeMappingSubject::Atom(b), external.clone()).is_err());
	tx.commit()?;
	let mut tx = store.begin();
	assert_eq!(
		tx.bridge_mapping_local(database, &subject)?.map(|row| row.reference()),
		Some(reference)
	);
	assert_eq!(
		tx.bridge_mapping_external(&external, BridgeMappingRole::Atom)?.map(|row| row.reference()),
		Some(reference)
	);
	assert_eq!(
		tx.bridge_mapping(reference)?.map(|row| row.state()),
		Some(BridgeMappingState::Ready)
	);
	Ok(())
}

#[test]
fn bridge_registry_preserves_snapshot_and_abort() -> FerrosGrynnResult<()> {
	let (store, database, a, _) = fixture()?;
	let subject = BridgeMappingSubject::Atom(a);
	let mut old = store.begin();
	assert!(old.bridge_mapping_local(database, &subject)?.is_none());
	let mut tx = store.begin();
	let reference = tx.register_bridge_mapping(subject.clone(), BridgeEntityRef {
		database,
		record: "profile:a".parse()?,
	})?;
	drop(tx);
	assert!(store.begin().bridge_mapping(reference)?.is_none());
	let mut tx = store.begin();
	tx.register_bridge_mapping(subject.clone(), BridgeEntityRef {
		database,
		record: "profile:a".parse()?,
	})?;
	tx.commit()?;
	assert!(old.bridge_mapping_local(database, &subject)?.is_none());
	assert!(store.begin().bridge_mapping_local(database, &subject)?.is_some());
	Ok(())
}

#[test]
fn bridge_registry_concurrent_unique_reservations_conflict() -> FerrosGrynnResult<()> {
	let (store, database, a, b) = fixture()?;
	let mut one = store.begin();
	let mut two = store.begin();
	let external = BridgeEntityRef {
		database,
		record: "profile:shared".parse()?,
	};
	one.register_bridge_mapping(BridgeMappingSubject::Atom(a), external.clone())?;
	two.register_bridge_mapping(BridgeMappingSubject::Atom(b), external)?;
	one.commit()?;
	assert!(two.commit().is_err());
	Ok(())
}

#[test]
fn bridge_registry_tombstones_deleted_subjects_and_rejects_foreign_refs() -> FerrosGrynnResult<()> {
	let (store, database, a, _) = fixture()?;
	let mut tx = store.begin();
	let subject = BridgeMappingSubject::Atom(a.clone());
	let external = BridgeEntityRef {
		database,
		record: "profile:a".parse()?,
	};
	let reference = tx.register_bridge_mapping(subject.clone(), external.clone())?;
	tx.commit()?;
	let mut tx = store.begin();
	tx.delete_atom(&a)?;
	tx.commit()?;
	let mut tx = store.begin();
	assert_eq!(
		tx.bridge_mapping(reference)?.map(|row| row.state()),
		Some(BridgeMappingState::Tombstoned)
	);
	assert!(tx.bridge_mapping_local(database, &subject)?.is_none());
	assert!(tx.bridge_mapping_external(&external, BridgeMappingRole::Atom)?.is_none());
	assert!(Datastore::in_memory().begin().bridge_mapping(reference).is_err());
	assert_eq!(BridgeMappingRef::try_from(Value::from(reference))?, reference);
	Ok(())
}

#[test]
fn bridge_registry_rejects_unrecoverable_external_names() -> FerrosGrynnResult<()> {
	let (store, database, record, _) = fixture()?;
	for class in ["x".repeat(4097), "profile\0bad".to_owned()] {
		let external = RecordId::from_value(class, Value::from("a"))?;
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
		tx.commit()?;
	}
	let oversized = RecordId::from_value(
		"profile",
		Value::Array(vec![Value::from("x".repeat(16_384))].into()),
	)?;
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
			.bridge_mapping_local(database, &BridgeMappingSubject::Atom(record))?
			.is_none()
	);
	Ok(())
}

#[test]
fn bridge_sources_reject_shapes_and_memory_before_receiving_writes() -> FerrosGrynnResult<()> {
	let runtime =
		tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| {
			crate::gql_status::FerrosGrynnError::bridge_operation_error("test runtime failed")
				.with_technical_source(error)
		})?;
	runtime.block_on(async {
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
		let mut options = crate::gql::QueryExecutionOptions::default();
		options.barrier_memory_bytes = std::num::NonZeroUsize::new(512)
			.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("test budget"))?;
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
	})
}

#[test]
fn bridge_registry_local_reopen_preserves_both_directions() -> FerrosGrynnResult<()> {
	use crate::datastore::{CheckpointMode, LocalOptions};
	let directory =
		std::env::temp_dir().join(format!("fgb30-{:016x}", uuid::Uuid::now_v7().as_u128() as u64));
	let mut options = LocalOptions::default();
	options.buffer_pool_pages = 64;
	let store = Datastore::create_local(&directory, options)?;
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
	let record: RecordId = "person:a".parse()?;
	tx.create_atom(record.clone(), PropertyMap::new())?;
	let database = tx.register_bridge_database(
		BridgeDatabaseOptions::new("sdb", "fake", "test", "test")?
			.map_class("person", "profile")?,
	)?;
	let subject = BridgeMappingSubject::Atom(record.clone());
	let external = BridgeEntityRef {
		database,
		record: "profile:a".parse()?,
	};
	let reference = tx.register_bridge_mapping(subject.clone(), external.clone())?;
	tx.commit()?;
	store.checkpoint(CheckpointMode::Passive)?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	assert_eq!(store.bridge_database("sdb")?.map(|database| database.reference()), Some(database));
	assert_eq!(
		store.begin().bridge_mapping_local(database, &subject)?.map(|row| row.reference()),
		Some(reference)
	);
	assert_eq!(
		store
			.begin()
			.bridge_mapping_external(&external, BridgeMappingRole::Atom)?
			.map(|row| row.reference()),
		Some(reference)
	);
	let mut tx = store.begin();
	tx.delete_atom(&record)?;
	tx.commit()?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	assert_eq!(
		store.begin().bridge_mapping(reference)?.map(|row| row.state()),
		Some(BridgeMappingState::Tombstoned)
	);
	drop(store);
	std::fs::remove_dir_all(&directory).map_err(|error| {
		crate::gql_status::FerrosGrynnError::bridge_operation_error("test directory cleanup failed")
			.with_technical_source(error)
	})?;
	Ok(())
}

struct ConstantBridge(crate::values::Value);
impl Bridge for ConstantBridge {
	fn execute(&self, _operation: BridgeOperation, context: BridgeContext) -> BridgeFuture<'_> {
		Box::pin(async move {
			context.check()?;
			Ok(self.0.clone())
		})
	}
}

#[test]
fn bridge_registry_roles_databases_occurrences_and_ssi() -> FerrosGrynnResult<()> {
	use crate::graph::{EdgeTopology, GraphKind};
	let (store, database, a, b) = fixture()?;
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("link", AtomKind::Edge, [])?)?;
	let edge: RecordId = "link:r".parse()?;
	tx.create_atom(edge.clone(), PropertyMap::new())?;
	let second =
		tx.register_bridge_database(BridgeDatabaseOptions::new("other", "fake", "test", "test")?)?;
	let g = tx.define_graph("g", GraphKind::Graph)?;
	let h = tx.define_graph("h", GraphKind::Graph)?;
	for graph in [g.id(), h.id()] {
		let source = tx.attach_node(graph, &a, None, None)?;
		let target = tx.attach_node(graph, &b, None, None)?;
		tx.attach_edge(graph, &edge, None, EdgeTopology::directed([source], [target]), None, None)?;
	}
	let entity = BridgeEntityRef {
		database,
		record: "foreign:shared".parse()?,
	};
	let atom =
		tx.register_bridge_mapping(BridgeMappingSubject::Atom(edge.clone()), entity.clone())?;
	let relation = tx.register_bridge_mapping(
		BridgeMappingSubject::Relation {
			graph: g.id(),
			edge: edge.clone(),
		},
		entity.clone(),
	)?;
	let occurrence = tx.register_bridge_mapping(
		BridgeMappingSubject::Relation {
			graph: h.id(),
			edge: edge.clone(),
		},
		BridgeEntityRef {
			database,
			record: "foreign:h".parse()?,
		},
	)?;
	let other =
		tx.register_bridge_mapping(BridgeMappingSubject::Atom(edge.clone()), BridgeEntityRef {
			database: second,
			record: entity.record.clone(),
		})?;
	assert_ne!(atom, relation);
	assert_ne!(atom, other);
	tx.commit()?;
	let mut tx = store.begin();
	tx.remove_graph("g")?;
	tx.commit()?;
	let mut tx = store.begin();
	assert_eq!(
		tx.bridge_mapping(relation)?.map(|row| row.state()),
		Some(BridgeMappingState::Tombstoned)
	);
	for reference in [atom, other, occurrence] {
		assert_eq!(
			tx.bridge_mapping(reference)?.map(|row| row.state()),
			Some(BridgeMappingState::Ready)
		);
	}
	let mut reader = store.begin_with(crate::datastore::TransactionOptions::new(
		crate::datastore::IsolationLevel::Serializable,
		crate::datastore::TransactionAccess::ReadWrite,
	));
	let subject = BridgeMappingSubject::Atom(a.clone());
	assert!(reader.bridge_mapping_local(database, &subject)?.is_none());
	let mut writer = store.begin();
	writer.register_bridge_mapping(subject, BridgeEntityRef {
		database,
		record: "profile:a".parse()?,
	})?;
	writer.commit()?;
	for result in reader.execute("UPDATE person:b SET probe = 1", &Object::new())? {
		result.result?;
	}
	assert!(reader.commit().is_err());
	Ok(())
}

#[test]
fn bridge_registry_pending_overflow_recovery_and_import_tombstone() -> FerrosGrynnResult<()> {
	use crate::{
		datastore::LocalOptions,
		graph::{EdgeTopology, GraphKind},
	};
	let directory =
		std::env::temp_dir().join(format!("fgb30p-{:016x}", uuid::Uuid::now_v7().as_u128() as u64));
	let mut options = LocalOptions::default();
	options.buffer_pool_pages = 64;
	options.maintenance.max_memory_bytes = std::num::NonZeroUsize::new(72 * 1024 * 1024)
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument("test maintenance budget")
		})?;
	let store = Datastore::create_local(&directory, options)?;
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
	tx.define_class(ClassSchema::new("link", AtomKind::Edge, [])?)?;
	let g = tx.define_graph("g", GraphKind::Graph)?;
	let a: RecordId = "person:a".parse()?;
	let b: RecordId = "person:b".parse()?;
	let edge: RecordId = "link:r".parse()?;
	let spare: RecordId = "link:spare".parse()?;
	for record in [&a, &b, &edge, &spare] {
		tx.create_atom(record.clone(), PropertyMap::new())?;
	}
	let source = tx.attach_node(g.id(), &a, None, None)?;
	let target = tx.attach_node(g.id(), &b, None, None)?;
	tx.attach_edge(g.id(), &edge, None, EdgeTopology::directed([source], [target]), None, None)?;
	tx.attach_edge(g.id(), &spare, None, EdgeTopology::directed([source], [target]), None, None)?;
	let database = tx.register_bridge_database(
		BridgeDatabaseOptions::new("sdb", "fake", "ns", "db")?.map_graph(g.id(), "relations")?,
	)?;
	let external = BridgeEntityRef {
		database,
		record: "relations:r".parse()?,
	};
	let subject = BridgeMappingSubject::Relation {
		graph: g.id(),
		edge,
	};
	let intent = BridgeMirrorRequest {
		relation: external.record.clone(),
		source: "profile:a".parse()?,
		target: "profile:b".parse()?,
		properties: Object::from_iter([("payload", Value::from("x".repeat(3 * 1024 * 1024)))]),
	};
	let reference = tx.reserve_bridge_mapping(subject.clone(), external.clone(), intent)?;
	tx.commit()?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	let row = store.begin().bridge_mapping(reference)?.ok_or_else(|| {
		crate::gql_status::FerrosGrynnError::bridge_mapping_error("test mapping absent")
	})?;
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
		})?
		.clone();
	intent.relation = "relations:spare".parse()?;
	let rejected = tx.reserve_bridge_mapping(
		BridgeMappingSubject::Relation {
			graph: g.id(),
			edge: spare,
		},
		BridgeEntityRef {
			database,
			record: intent.relation.clone(),
		},
		intent,
	)?;
	assert!(
		tx.commit().is_err(),
		"publication must reject a registry that cannot reopen within the same budget"
	);
	assert!(store.begin().bridge_mapping(rejected)?.is_none());
	let mut tx = store.begin();
	tx.confirm_bridge_mapping(reference)?;
	tx.commit()?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	assert_eq!(
		store.begin().bridge_mapping(reference)?.map(|row| row.state()),
		Some(BridgeMappingState::Ready)
	);
	for result in store.execute(
		"BEGIN IMPORT; DELETE person:a CASCADE; END IMPORT WITH VALIDATION;",
		&Object::new(),
	)? {
		result.result?;
	}
	let mut tx = store.begin();
	tx.define_parameter(crate::datastore::SchemaParameter::new(
		"catalog_probe",
		Value::from(1_i64),
	)?)?;
	tx.commit()?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	assert_eq!(
		store.begin().bridge_mapping(reference)?.map(|row| row.state()),
		Some(BridgeMappingState::Tombstoned)
	);
	assert!(store.begin().bridge_mapping_local(database, &subject)?.is_none());
	assert!(
		store.begin().bridge_mapping_external(&external, BridgeMappingRole::Relation)?.is_none()
	);
	drop(store);
	std::fs::remove_dir_all(&directory).map_err(|error| {
		crate::gql_status::FerrosGrynnError::bridge_operation_error("test cleanup failed")
			.with_technical_source(error)
	})?;
	Ok(())
}

#[test]
fn bridge_query_sources_preserve_values_and_override_bindings() -> FerrosGrynnResult<()> {
	let runtime =
		tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| {
			crate::gql_status::FerrosGrynnError::bridge_operation_error("test runtime failed")
				.with_technical_source(error)
		})?;
	runtime.block_on(async {
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
	})
}

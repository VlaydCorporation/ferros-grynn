//! Fake-client operator tests isolate local atomicity from independent IO.

use std::{
	collections::BTreeMap,
	sync::{
		Arc,
		atomic::{AtomicBool, AtomicUsize, Ordering},
	},
};

use parking_lot::Mutex;

use super::*;
use crate::{
	datastore::{ClassSchema, Datastore},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomKind, GraphKind},
	values::{Object, PropertyMap, RecordId, Value},
};

#[derive(Clone)]
struct Fake {
	database: BridgeDatabaseRef,
	rows: Arc<Mutex<BTreeMap<RecordId, Value>>>,
	mirrors: Arc<AtomicUsize>,
	unknown: Arc<AtomicBool>,
	batches: Arc<AtomicUsize>,
}
impl Bridge for Fake {
	fn database(&self) -> Option<BridgeDatabaseRef> {
		Some(self.database)
	}

	fn capabilities(&self) -> BridgeCapabilities {
		BridgeCapabilities::FULL
	}

	fn execute(&self, operation: BridgeOperation, context: BridgeContext) -> BridgeFuture<'_> {
		Box::pin(async move {
			context.check()?;
			match operation {
				BridgeOperation::Records(records) => {
					self.batches.fetch_add(1, Ordering::Relaxed);
					Ok(Value::Array(
						records
							.into_iter()
							.map(|record| {
								self.rows.lock().get(&record).cloned().unwrap_or(Value::None)
							})
							.collect(),
					))
				}
				BridgeOperation::Mirror(intent) => {
					self.mirrors.fetch_add(1, Ordering::Relaxed);
					let mut object = intent.properties;
					object.insert("id", Value::Record(Box::new(intent.relation.clone())));
					object.insert("in", Value::Record(Box::new(intent.source)));
					object.insert("out", Value::Record(Box::new(intent.target)));
					let value = Value::Object(object);
					let mut rows = self.rows.lock();
					if let Some(existing) = rows.get(&intent.relation) {
						if existing != &value {
							return Err(FerrosGrynnError::bridge_mapping_error(
								"fake relation conflict",
							));
						}
					} else {
						rows.insert(intent.relation, value.clone());
					}
					if self.unknown.swap(false, Ordering::Relaxed) {
						return Err(FerrosGrynnError::bridge_operation_error(
							"unknown external commit outcome",
						));
					}
					Ok(value)
				}
				_ => Err(FerrosGrynnError::bridge_operation_error("fake source not configured")),
			}
		})
	}
}
fn fixture() -> FerrosGrynnResult<(Datastore, Fake)> {
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
	tx.define_class(ClassSchema::new("link", AtomKind::Edge, [])?)?;
	let graph = tx.define_graph("g", GraphKind::Graph)?;
	let database = tx.register_bridge_database(
		BridgeDatabaseOptions::new("sdb", "fake", "test", "test")?
			.map_class("person", "profile")?
			.map_class("link", "edge_profile")?
			.map_graph(graph.id(), "relations")?,
	)?;
	let rows = Arc::new(Mutex::new(BTreeMap::new()));
	for (key, rank) in [("a", 10_i64), ("b", 20)] {
		let record = RecordId::from_value("person", Value::from(key))?;
		let external = RecordId::from_value("profile", Value::from(key))?;
		tx.create_atom(record.clone(), PropertyMap::new())?;
		tx.attach_node(graph.id(), &record, None, None)?;
		tx.register_bridge_mapping(BridgeMappingSubject::Atom(record), BridgeEntityRef {
			database,
			record: external.clone(),
		})?;
		rows.lock().insert(
			external.clone(),
			Value::Object(Object::from_iter([
				("id", Value::Record(Box::new(external))),
				("rank", Value::from(rank)),
			])),
		);
	}
	tx.commit()?;
	let fake = Fake {
		database,
		rows,
		mirrors: Arc::new(AtomicUsize::new(0)),
		unknown: Arc::new(AtomicBool::new(false)),
		batches: Arc::new(AtomicUsize::new(0)),
	};
	store.attach_bridge(database, fake.clone())?;
	Ok((store, fake))
}
fn runtime() -> FerrosGrynnResult<tokio::runtime::Runtime> {
	tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| {
		FerrosGrynnError::bridge_operation_error("test runtime failed").with_technical_source(error)
	})
}

#[test]
fn bridge_fetch_precedes_order_and_pagination() -> FerrosGrynnResult<()> {
	runtime()?.block_on(async {
		let (store, _) = fixture()?;
		let result = store
			.query(
				"SELECT id AS p FROM person FETCH p WITH BRIDGE sdb ORDER BY \
				 p.external.sdb.properties.rank DESC LIMIT 1;",
			)
			.execute()
			.await?;
		let value = result.results[0].result.clone()?.into_parts().0;
		let Value::Array(rows) = value else {
			return Err(FerrosGrynnError::invalid_argument("test rows absent"));
		};
		assert_eq!(rows.len(), 1);
		let Value::Object(row) = rows
			.iter()
			.next()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("test row absent"))?
		else {
			return Err(FerrosGrynnError::invalid_argument("test row absent"));
		};
		let Some(Value::Object(person)) = row.get("p") else {
			return Err(FerrosGrynnError::invalid_argument("fetched person absent"));
		};
		assert_eq!(person.get("id"), Some(&Value::Record(Box::new("person:b".parse()?))));
		assert!(
			store
				.query("SELECT id AS p FROM person FETCH p WITH BRIDGE missing LIMIT 0")
				.execute()
				.await?
				.results[0]
				.result
				.is_err()
		);
		Ok(())
	})
}

#[test]
fn bridge_nested_fetch_reuses_cache_and_merges_database_namespaces() -> FerrosGrynnResult<()> {
	runtime()?.block_on(async {
		let (store, fake) = fixture()?;
		let mut tx = store.begin();
		let database = tx.register_bridge_database(
			BridgeDatabaseOptions::new("other", "fake", "test", "test")?
				.map_class("person", "profile")?,
		)?;
		for key in ["a", "b"] {
			tx.register_bridge_mapping(
				BridgeMappingSubject::Atom(RecordId::from_value("person", Value::from(key))?),
				BridgeEntityRef {
					database,
					record: RecordId::from_value("profile", Value::from(key))?,
				},
			)?;
		}
		tx.commit()?;
		let mut other = fake.clone();
		other.database = database;
		store.attach_bridge(database, other)?;
		let result = store
			.query(
				"SELECT {nested: [id, id]} AS box FROM person FETCH box.nested WITH BRIDGE sdb, \
				 box.nested WITH BRIDGE other ORDER BY id",
			)
			.execute()
			.await?;
		let Value::Array(rows) = result.results[0].result.clone()?.into_parts().0 else {
			return Err(FerrosGrynnError::invalid_argument("test rows absent"));
		};
		assert_eq!(rows.len(), 2);
		for row in rows {
			let Value::Object(row) = row else {
				return Err(FerrosGrynnError::invalid_argument("test row absent"));
			};
			let Some(Value::Object(object)) = row.get("box") else {
				return Err(FerrosGrynnError::invalid_argument("test box absent"));
			};
			let Some(Value::Array(nested)) = object.get("nested") else {
				return Err(FerrosGrynnError::invalid_argument("test nested array absent"));
			};
			assert_eq!(nested.len(), 2);
			for value in nested {
				let Value::Object(value) = value else {
					return Err(FerrosGrynnError::invalid_argument("test fetched value absent"));
				};
				let Some(Value::Object(external)) = value.get("external") else {
					return Err(FerrosGrynnError::invalid_argument("test namespaces absent"));
				};
				assert!(external.contains_key("sdb") && external.contains_key("other"));
			}
		}
		assert_eq!(fake.batches.load(Ordering::Relaxed), 4);
		let before = fake.batches.load(Ordering::Relaxed);
		let explain = store
			.query("EXPLAIN SELECT id AS p FROM person FETCH p WITH BRIDGE sdb")
			.execute()
			.await?;
		explain.results[0].result.clone()?;
		assert_eq!(fake.batches.load(Ordering::Relaxed), before);
		let bad = store
			.query("SELECT {id: id, external: 7} AS p FROM person FETCH p WITH BRIDGE sdb")
			.execute()
			.await?;
		assert!(bad.results[0].result.is_err());
		Ok(())
	})
}

#[test]
fn bridge_relate_partial_commit_resume_and_cancel() -> FerrosGrynnResult<()> {
	tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.max_blocking_threads(1)
		.build()
		.map_err(|error| {
			FerrosGrynnError::bridge_operation_error("test runtime failed")
				.with_technical_source(error)
		})?
		.block_on(async {
			let (store, fake) = fixture()?;
			let query = "RELATE (person:a)-[link:r]->(person:b) IN GRAPH g WITH BRIDGE sdb;";
			let cancelled = store.query(&format!("BEGIN; {query} CANCEL;")).execute().await?;
			for result in cancelled.results {
				result.result?;
			}
			assert_eq!(fake.mirrors.load(Ordering::Relaxed), 0);
			assert!(store.begin().atom(&"link:r".parse()?)?.is_none());
			fake.unknown.store(true, Ordering::Relaxed);
			let result = tokio::time::timeout(
				std::time::Duration::from_secs(3),
				store.query(query).execute(),
			)
			.await
			.map_err(|error| {
				FerrosGrynnError::bridge_operation_error("single worker deadlock")
					.with_technical_source(error)
			})??;
			result.results[0].result.clone()?;
			assert_eq!(result.effects.len(), 1);
			assert!(result.effects[0].result.is_err());
			let reference = result.effects[0].mapping;
			assert_eq!(
				store.begin().bridge_mapping(reference)?.map(|row| row.state()),
				Some(BridgeMappingState::Pending)
			);
			assert!(store.begin().atom(&"link:r".parse()?)?.is_some());
			assert_eq!(store.resume_bridge_mapping(reference).await?, BridgeMappingState::Ready);
			assert_eq!(fake.mirrors.load(Ordering::Relaxed), 2);
			assert_eq!(store.resume_bridge_mapping(reference).await?, BridgeMappingState::Ready);
			assert_eq!(fake.mirrors.load(Ordering::Relaxed), 2);
			let next = store
				.query(
					"RELATE (person:a)-[link:next]->(person:b) IN GRAPH g WITH BRIDGE sdb; SELECT \
					 id AS edge FROM link:next IN GRAPH g FETCH edge WITH BRIDGE sdb;",
				)
				.execute()
				.await?;
			assert_eq!(next.effects.len(), 1);
			for result in next.results {
				result.result?;
			}
			assert_eq!(fake.mirrors.load(Ordering::Relaxed), 3);
			let connector = store
				.query(
					"RELATE ATOM <[link:r]>~[link:connector]~><[link:next]> IN GRAPH g WITH \
					 BRIDGE sdb",
				)
				.execute()
				.await?;
			connector.results[0].result.clone()?;
			assert!(connector.effects[0].result.is_ok());
			let recipes = Value::Array(
				fake.rows
					.lock()
					.values()
					.filter(
						|value| matches!(value, Value::Object(object) if object.contains_key("in")),
					)
					.cloned()
					.collect(),
			);
			let result = store
				.query("RETURN GRAPH {nodes: [], edges: $recipes} WITH BRIDGE sdb")
				.bind(Object::from_iter([("recipes", recipes.clone())]))
				.execute()
				.await?;
			let Value::Graph(graph) = result.results[0].result.clone()?.into_parts().0 else {
				return Err(FerrosGrynnError::invalid_argument("graph result absent"));
			};
			assert_eq!(graph.node_count(), 2);
			assert_eq!(graph.edge_count(), 3);
			let result = store
				.query("BUILD GRAPH imported FROM $recipes WITH BRIDGE sdb")
				.bind(Object::from_iter([("recipes", recipes)]))
				.execute()
				.await?;
			result.results[0].result.clone()?;
			assert_eq!(
				store.begin().bridge_mapping(reference)?.map(|row| row.state()),
				Some(BridgeMappingState::Ready)
			);
			let recipes = Value::Array(
				fake.rows
					.lock()
					.values()
					.filter(
						|value| matches!(value, Value::Object(object) if object.contains_key("in")),
					)
					.cloned()
					.collect(),
			);
			let result = store
				.query("BUILD GRAPH OVERWRITE g FROM $recipes WITH BRIDGE sdb")
				.bind(Object::from_iter([("recipes", recipes)]))
				.execute()
				.await?;
			result.results[0].result.clone()?;
			assert_eq!(
				store.begin().bridge_mapping(reference)?.map(|row| row.state()),
				Some(BridgeMappingState::Ready)
			);
			Ok(())
		})
}

struct Waiting {
	database: BridgeDatabaseRef,
	started: Arc<AtomicBool>,
	dropped: Arc<AtomicBool>,
}

#[test]
fn bridge_graph_recipes_validate_loops_duplicates_and_atomic_publication() -> FerrosGrynnResult<()>
{
	runtime()?.block_on(async {
		let (store, fake) = fixture()?;
		let result = store
			.query("RELATE (person:a)-[link:loop]->(person:a) IN GRAPH g WITH BRIDGE sdb")
			.execute()
			.await?;
		result.results[0].result.clone()?;
		let recipes = Value::Array(
			fake.rows
				.lock()
				.values()
				.filter(|value| matches!(value, Value::Object(object) if object.contains_key("in")))
				.cloned()
				.collect(),
		);
		for family in ["<FOREST>", "<TREE>", "<ROOTED_TREE> ROOT person:a", "<DAG>"] {
			let query =
				format!("RETURN GRAPH {family} {{nodes: [], edges: $recipes}} WITH BRIDGE sdb");
			let result = store
				.query(&query)
				.bind(Object::from_iter([("recipes", recipes.clone())]))
				.execute()
				.await?;
			assert!(result.results[0].result.is_err(), "{family} must reject this loop");
		}
		let Value::Array(mut duplicate) = recipes.clone() else {
			return Err(FerrosGrynnError::invalid_argument("test recipes absent"));
		};
		let first = duplicate
			.iter()
			.next()
			.cloned()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("test recipe absent"))?;
		duplicate.push(first);
		let result = store
			.query("RETURN GRAPH {nodes: [], edges: $recipes} WITH BRIDGE sdb")
			.bind(Object::from_iter([("recipes", Value::Array(duplicate))]))
			.execute()
			.await?;
		let Value::Graph(graph) = result.results[0].result.clone()?.into_parts().0 else {
			return Err(FerrosGrynnError::invalid_argument("test graph absent"));
		};
		assert_eq!(graph.node_count(), 1);
		assert_eq!(graph.edge_count(), 1);
		let invalid = Value::Array(
			vec![Value::Object(Object::from_iter([
				("id", Value::Record(Box::new("relations:missing".parse()?))),
				("in", Value::Record(Box::new("profile:a".parse()?))),
				("out", Value::Record(Box::new("profile:b".parse()?))),
			]))]
			.into(),
		);
		let result = store
			.query("BUILD GRAPH rejected FROM $recipes WITH BRIDGE sdb")
			.bind(Object::from_iter([("recipes", invalid)]))
			.execute()
			.await?;
		assert!(result.results[0].result.is_err());
		assert!(store.begin().named_graph("rejected")?.is_none());
		Ok(())
	})
}
struct DropProbe(Arc<AtomicBool>);
impl Drop for DropProbe {
	fn drop(&mut self) {
		self.0.store(true, Ordering::Release);
	}
}
impl Bridge for Waiting {
	fn database(&self) -> Option<BridgeDatabaseRef> {
		Some(self.database)
	}

	fn capabilities(&self) -> BridgeCapabilities {
		BridgeCapabilities::FULL
	}

	fn execute(&self, _operation: BridgeOperation, _context: BridgeContext) -> BridgeFuture<'_> {
		Box::pin(async move {
			let _probe = DropProbe(self.dropped.clone());
			self.started.store(true, Ordering::Release);
			std::future::pending().await
		})
	}
}

#[test]
fn bridge_deadline_and_drop_release_the_dispatcher() -> FerrosGrynnResult<()> {
	runtime()?.block_on(async {
		let (store, fake) = fixture()?;
		let started = Arc::new(AtomicBool::new(false));
		let dropped = Arc::new(AtomicBool::new(false));
		store.attach_bridge(fake.database, Waiting {
			database: fake.database,
			started: started.clone(),
			dropped: dropped.clone(),
		})?;
		let mut options = crate::gql::QueryExecutionOptions::default();
		options.statement_timeout = Some(std::time::Duration::from_millis(30));
		let result = store
			.query("SELECT id AS p FROM person FETCH p WITH BRIDGE sdb")
			.options(options)
			.execute()
			.await?;
		assert!(result.results[0].result.is_err());
		assert!(started.load(Ordering::Acquire));
		assert!(dropped.load(Ordering::Acquire));
		started.store(false, Ordering::Release);
		dropped.store(false, Ordering::Release);
		let result = tokio::time::timeout(
			std::time::Duration::from_millis(30),
			store.query("SELECT id AS p FROM person FETCH p WITH BRIDGE sdb").execute(),
		)
		.await;
		assert!(result.is_err());
		assert!(started.load(Ordering::Acquire));
		assert!(dropped.load(Ordering::Acquire));
		store.attach_bridge(fake.database, fake)?;
		let result =
			store.query("SELECT id AS p FROM person FETCH p WITH BRIDGE sdb").execute().await?;
		result.results[0].result.clone()?;
		Ok(())
	})
}

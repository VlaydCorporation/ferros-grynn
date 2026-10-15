#![cfg(test)]

//! Public embedded-client consumer and runtime gates for Stage 30.

#![cfg(feature = "bridge-surreal")]
/// Assertions shared with the root unit-test harness.
#[path = "common/assertions.rs"]
pub mod test_assertions;

use std::sync::{
	Arc,
	atomic::{AtomicBool, Ordering},
};

use ferros_grynn::{
	bridge::{
		Bridge,
		BridgeCapabilities,
		BridgeContext,
		BridgeData,
		BridgeDatabaseOptions,
		BridgeDatabaseRef,
		BridgeEntityRef,
		BridgeFuture,
		BridgeMappingState,
		BridgeMappingSubject,
		BridgeOperation,
		FerrosBridge,
		SurrealBridge,
		SurrealQueryBridgeExt,
	},
	datastore::{ClassSchema, Datastore},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomKind, GraphKind},
	values::{PropertyMap, RecordId, Value},
};
use surrealdb::{Connection, IndexedResults, Surreal, types as sdk};

/// Reports an SDK fixture error and retains it as a technical source.
fn external(error: surrealdb::Error) -> FerrosGrynnError {
	eprintln!("SDK fixture error: {error}");
	FerrosGrynnError::bridge_operation_error("SDK integration failed").with_technical_source(error)
}
/// Extracts the only checked native statement result.
fn single(mut response: IndexedResults) -> FerrosGrynnResult<sdk::Value> {
	use crate::test_assertions::AssertComparison as _;

	(&response.num_statements(), &1_usize).assert_equal();
	response = response.check().map_err(external)?;
	response.take::<sdk::Value>(0).map_err(external)
}
/// Creates a current-thread Tokio runtime with its drivers enabled.
fn runtime() -> FerrosGrynnResult<tokio::runtime::Runtime> {
	tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| {
		FerrosGrynnError::bridge_operation_error("test runtime failed").with_technical_source(error)
	})
}

/// Bridge wrapper injecting one unknown commit outcome before forwarding
/// ordinary requests.
struct UnknownCommit<C: Connection> {
	/// Underlying value or owner wrapped by this adapter.
	inner: Arc<SurrealBridge<C>>,
	/// One-shot flag controlling the injected test failure.
	once: AtomicBool,
}
impl<C: Connection> Bridge for UnknownCommit<C> {
	/// Returns database for `UnknownCommit<C>`.
	fn database(&self) -> Option<BridgeDatabaseRef> {
		self.inner.database()
	}

	/// Returns capabilities for `UnknownCommit<C>`.
	fn capabilities(&self) -> BridgeCapabilities {
		BridgeCapabilities::FULL
	}

	/// Forwards requests and reports one unknown outcome after successful
	/// mirroring.
	fn execute(&self, operation: BridgeOperation, context: BridgeContext) -> BridgeFuture<'_> {
		let mirror = matches!(operation, BridgeOperation::Mirror(_));
		Box::pin(async move {
			let value = self.inner.execute(operation, context).await?;
			if mirror && self.once.swap(false, Ordering::Relaxed) {
				return Err(FerrosGrynnError::bridge_operation_error(
					"simulated unknown native commit",
				));
			}
			Ok(value)
		})
	}
}

/// Exercises both bridge directions, batched records and mirror recovery.
async fn embedded<C: Connection>(client: Surreal<C>) -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
	tx.define_class(ClassSchema::new("link", AtomKind::Edge, [])?)?;
	let graph = tx.define_graph("g", GraphKind::Graph)?;
	let record: RecordId = "person:a".parse()?;
	tx.create_atom(record.clone(), PropertyMap::new())?;
	let other: RecordId = "person:b".parse()?;
	tx.create_atom(other.clone(), PropertyMap::new())?;
	tx.attach_node(graph.id(), &record, None, None)?;
	tx.attach_node(graph.id(), &other, None, None)?;
	let reference = tx.register_bridge_database(
		BridgeDatabaseOptions::new("sdb", "surrealdb", "bridge", "test")?
			.map_class("person", "profile")?
			.map_graph(graph.id(), "relations")?,
	)?;
	tx.commit()?;
	let database = store
		.bridge_database("sdb")?
		.ok_or_else(|| FerrosGrynnError::bridge_mapping_error("test descriptor absent"))?;
	let bridge = SurrealBridge::new(client.clone(), database.clone()).await?;
	let result = store
		.query("RETURN $bridge")
		.use_bridge(
			bridge,
			BridgeData::ExternalQuery("RETURN [NONE, NULL, 9007199254740993]", None),
		)
		.execute()
		.await?;
	assert_eq!(
		result.results[0].result.clone()?.into_parts().0,
		Value::Array(vec![Value::None, Value::Null, Value::from(9_007_199_254_740_993_i64)].into())
	);
	let mut result = client
		.query("RETURN $bridge")
		.bind(("bridge", 99_i64))
		.use_bridge(
			FerrosBridge::new(store.clone()),
			BridgeData::InternalQuery("RETURN [none, null, 9007199254740993]", None),
		)
		.await?;
	assert_eq!(
		result.take::<sdk::Value>(0).map_err(external)?,
		sdk::Value::Array(
			vec![
				sdk::Value::None,
				sdk::Value::Null,
				sdk::Value::Number(sdk::Number::Int(9_007_199_254_740_993))
			]
			.into()
		)
	);
	assert!(
		client
			.query("RETURN 1")
			.use_bridge(
				FerrosBridge::new(store.clone()),
				BridgeData::InternalQuery("CREATE person:forbidden", None)
			)
			.await
			.is_err()
	);
	assert!(store.begin().atom(&"person:forbidden".parse()?)?.is_none());
	assert!(
		client
			.query("RETURN 1")
			.use_bridge(
				FerrosBridge::new(store.clone()),
				BridgeData::InternalQuery("SLEEP 1ms", None)
			)
			.await
			.is_err()
	);
	let bridge = Arc::new(SurrealBridge::new(client.clone(), database).await?);
	assert!(
		store
			.query("RETURN 1")
			.use_bridge(
				bridge.clone(),
				BridgeData::ExternalQuery("USE NS other DB other; RETURN 1", None)
			)
			.execute()
			.await
			.is_err()
	);
	client.use_ns("bridge").use_db("test").await.map_err(external)?;
	client
		.query("CREATE profile:a SET rank = 7; CREATE profile:b SET rank = 9")
		.await
		.map_err(external)?
		.check()
		.map_err(external)?;
	let mut tx = store.begin();
	tx.register_bridge_mapping(
		BridgeMappingSubject::Atom(record),
		BridgeEntityRef::new(reference, "profile:a".parse()?),
	)?;
	tx.commit()?;
	let mut tx = store.begin();
	tx.register_bridge_mapping(
		BridgeMappingSubject::Atom(other),
		BridgeEntityRef::new(reference, "profile:b".parse()?),
	)?;
	tx.commit()?;
	store.attach_bridge(reference, UnknownCommit {
		inner: bridge,
		once: AtomicBool::new(true),
	})?;
	let mirrored = store
		.query(
			"RELATE (person:a)-[link:r]->(person:b) IN GRAPH g CONTENT {nullable: null, optional: \
			 none} WITH BRIDGE sdb",
		)
		.execute()
		.await?;
	mirrored.results[0].result.clone()?;
	assert_eq!(mirrored.effects.len(), 1);
	assert!(mirrored.effects[0].result.is_err());
	let mapping = mirrored.effects[0].mapping;
	assert_eq!(
		store.begin().bridge_mapping(mapping)?.map(|row| row.state()),
		Some(BridgeMappingState::Pending)
	);
	assert_eq!(store.resume_bridge_mapping(mapping).await?, BridgeMappingState::Ready);
	assert_eq!(
		single(client.query("RETURN count((SELECT * FROM profile))").await.map_err(external)?)?,
		sdk::Value::Number(sdk::Number::Int(2))
	);
	let result = store
		.query("SELECT id AS relation FROM link:r IN GRAPH g FETCH relation WITH BRIDGE sdb")
		.execute()
		.await?;
	result.results[0].result.clone()?;
	let result = store
		.query("SELECT id AS person FROM person FETCH person WITH BRIDGE sdb")
		.execute()
		.await?;
	result.results[0].result.clone()?;
	let mapped = SurrealBridge::new(
		client,
		store
			.bridge_database("sdb")?
			.ok_or_else(|| FerrosGrynnError::bridge_mapping_error("descriptor absent"))?,
	)
	.await?;
	let result = store
		.query("RETURN $rank")
		.use_bridge(
			mapped,
			BridgeData::ExternalQuery(
				"RETURN {rank: 7}",
				Some(std::collections::HashMap::from([("rank", "rank")])),
			),
		)
		.execute()
		.await?;
	assert_eq!(result.results[0].result.clone()?.into_parts().0, Value::from(7_i64));
	Ok(())
}

/// Verifies bridge surreal mem both directions.
#[cfg(feature = "bridge-surreal-mem")]
#[test]
fn bridge_surreal_mem_both_directions() -> FerrosGrynnResult<()> {
	let result = runtime()?.block_on(async {
		embedded(Surreal::new::<surrealdb::engine::local::Mem>(()).await.map_err(external)?).await
	});
	if let Err(error) = &result
		&& let Some(source) = error.technical_source()
	{
		eprintln!("native fixture failure: {source}");
	}
	result
}

#[cfg(feature = "bridge-surreal-rocksdb")]
/// Keeps the SDK runtime alive until its asynchronously dropped engine closes.
async fn wait_native_close(directory: &std::path::Path) -> FerrosGrynnResult<()> {
	#[cfg(windows)]
	{
		use std::os::windows::fs::OpenOptionsExt;
		let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
		loop {
			match std::fs::OpenOptions::new()
				.read(true)
				.write(true)
				.share_mode(0)
				.open(directory.join("LOCK"))
			{
				Ok(file) => {
					drop(file);
					return Ok(());
				}
				Err(error)
					if matches!(error.raw_os_error(), Some(32 | 33))
						&& std::time::Instant::now() < deadline =>
				{
					tokio::time::sleep(std::time::Duration::from_millis(25)).await;
				}
				Err(error) => {
					return Err(FerrosGrynnError::bridge_operation_error(
						"native SDK shutdown failed",
					)
					.with_technical_source(error));
				}
			}
		}
	}
	#[cfg(not(windows))]
	{
		let _ = directory;
		// The SDK exposes no shutdown receipt; let its closed-channel tasks
		// run.
		tokio::time::sleep(std::time::Duration::from_millis(100)).await;
		Ok(())
	}
}

/// Verifies bridge surreal rocksdb both directions and reopen.
#[cfg(feature = "bridge-surreal-rocksdb")]
#[test]
fn bridge_surreal_rocksdb_both_directions_and_reopen() -> FerrosGrynnResult<()> {
	let directory =
		std::env::temp_dir().join(format!("sdb30-{:016x}", uuid::Uuid::now_v7().as_u128() as u64));
	runtime()?.block_on(async {
		let client = Surreal::new::<surrealdb::engine::local::RocksDb>(directory.clone())
			.await
			.map_err(external)?;
		embedded(client.clone()).await?;
		client.use_ns("bridge").use_db("test").await.map_err(external)?;
		client
			.query("CREATE retained:a SET value = 7")
			.await
			.map_err(external)?
			.check()
			.map_err(external)?;
		drop(client);
		wait_native_close(&directory).await?;
		let client = Surreal::new::<surrealdb::engine::local::RocksDb>(directory.clone())
			.await
			.map_err(external)?;
		client.use_ns("bridge").use_db("test").await.map_err(external)?;
		let value = single(client.query("RETURN retained:a.value").await.map_err(external)?)?;
		assert_eq!(value, sdk::Value::Number(sdk::Number::Int(7)));
		drop(client);
		wait_native_close(&directory).await?;
		Ok::<_, FerrosGrynnError>(())
	})?;
	std::fs::remove_dir_all(directory).map_err(|error| {
		FerrosGrynnError::bridge_operation_error("test cleanup failed").with_technical_source(error)
	})?;
	Ok(())
}

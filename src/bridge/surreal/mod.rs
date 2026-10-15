//! Embedded SurrealDB adapter and an extension of the native query builder.

mod conversion;
#[cfg(test)]
mod tests;

use std::{collections::BTreeSet, future::IntoFuture, pin::Pin, sync::Arc};

use surrealdb::{Connection, IndexedResults, Surreal, method::Query, types as sdk};

use super::{
	Bridge,
	BridgeCapabilities,
	BridgeContext,
	BridgeData,
	BridgeDatabase,
	BridgeDatabaseRef,
	BridgeFuture,
	BridgeMirrorRequest,
	BridgeOperation,
};
use crate::{
	gql::QueryExecutionOptions,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{Object, Value},
};

/// Retains SDK diagnostic detail in the redacted technical source chain.
fn external(error: surrealdb::Error) -> FerrosGrynnError {
	FerrosGrynnError::bridge_operation_error("embedded SurrealDB operation failed")
		.with_technical_source(error)
}
/// Rejects multiple resulting statements before the receiving query can run.
fn single(mut response: IndexedResults) -> FerrosGrynnResult<sdk::Value> {
	if response.num_statements() != 1 {
		return Err(FerrosGrynnError::bridge_operation_error(
			"bridge source requires exactly one resulting statement",
		));
	}
	response = response.check().map_err(external)?;
	response.take::<sdk::Value>(0).map_err(external)
}

/// Client-only adapter; identity and table correspondences remain owned
/// metadata.
#[doc = include_str!("../../../docs/rustdoc/bridge-surreal-example-01.md")]
pub struct SurrealBridge<C: Connection> {
	/// Runtime-only dedicated native session.
	client: Surreal<C>,
	/// Stable descriptor qualifying namespace, database and table mappings.
	database: BridgeDatabase,
}
impl<C: Connection> SurrealBridge<C> {
	/// Configures a dedicated SDK session for the exact registered native
	/// database.
	///
	/// # Errors
	/// Returns invalid metadata or the native namespace/database selection
	/// error.
	pub async fn new(client: Surreal<C>, database: BridgeDatabase) -> FerrosGrynnResult<Self> {
		if database.namespace().is_empty() || database.database().is_empty() {
			return Err(FerrosGrynnError::bridge_mapping_error(
				"SurrealDB namespace/database must not be empty",
			));
		}
		let client = client.clone();
		client
			.use_ns(database.namespace().as_str())
			.use_db(database.database().as_str())
			.await
			.map_err(external)?;
		Ok(Self {
			client,
			database,
		})
	}

	/// Executes native IO and then performs the admitted typed conversion.
	async fn operation(
		&self,
		operation: BridgeOperation,
		context: &BridgeContext,
	) -> FerrosGrynnResult<Value> {
		context.check()?;
		let value = match operation {
			BridgeOperation::Query {
				source,
				variables,
			} => {
				let bindings = conversion::outgoing(Value::Object(variables), context, 0)?;
				let client = self.client.clone();
				single(client.query(source.into_string()).bind(bindings).await.map_err(external)?)?
			}
			BridgeOperation::RecordOrClass(source) => {
				context.admit_bytes(source.len().saturating_mul(16))?;
				let native = surrealdb::parse::value(&source).map_err(|error| {
					FerrosGrynnError::bridge_operation_error("invalid native record/class source")
						.with_technical_source(std::io::Error::other(error.into_boxed_dyn_error()))
				})?;
				if !matches!(native, sdk::Value::Table(_) | sdk::Value::RecordId(_)) {
					return Err(FerrosGrynnError::bridge_operation_error(
						"RecordOrClass source is not a native identity",
					));
				}
				single(
					self.client
						.query("SELECT * FROM $source")
						.bind(("source", native))
						.await
						.map_err(external)?,
				)?
			}
			BridgeOperation::Records(records) => {
				let ids = records
					.into_iter()
					.map(|record| conversion::record_out(record, context, 0))
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
				let values = sdk::Value::Array(
					ids.iter().cloned().map(sdk::Value::RecordId).collect::<Vec<_>>().into(),
				);
				let response = single(
					self.client
						.query("SELECT * FROM $records")
						.bind(("records", values))
						.await
						.map_err(external)?,
				)?;
				let sdk::Value::Array(rows) = response else {
					return Err(FerrosGrynnError::bridge_operation_error(
						"external record batch is not an array",
					));
				};
				let mut by_id = std::collections::BTreeMap::new();
				for row in rows {
					let sdk::Value::Object(object) = &row else {
						return Err(FerrosGrynnError::bridge_operation_error(
							"external batch record is not an object",
						));
					};
					let Some(sdk::Value::RecordId(id)) = object.get("id") else {
						return Err(FerrosGrynnError::bridge_operation_error(
							"external batch record lacks typed id",
						));
					};
					if by_id.insert(id.clone(), row).is_some() {
						return Err(FerrosGrynnError::bridge_operation_error(
							"external batch has duplicate identities",
						));
					}
				}
				sdk::Value::Array(
					ids.into_iter()
						.map(|id| by_id.remove(&id).unwrap_or(sdk::Value::None))
						.collect::<Vec<_>>()
						.into(),
				)
			}
			BridgeOperation::Mirror(intent) => self.mirror(intent, context).await?,
		};
		context.check()?;
		conversion::incoming(value, context, 0)
	}

	/// Verifies or creates the exact reserved relation without endpoint writes.
	async fn mirror(
		&self,
		intent: BridgeMirrorRequest,
		context: &BridgeContext,
	) -> FerrosGrynnResult<sdk::Value> {
		if !self.database.graphs.values().any(|table| table.as_str() == intent.relation.class()) {
			return Err(FerrosGrynnError::bridge_mapping_error(
				"mirror relation table has no graph mapping",
			));
		}
		if intent.properties.keys().any(|key| matches!(key.as_str(), "id" | "in" | "out")) {
			return Err(FerrosGrynnError::bridge_operation_error(
				"mirror properties contain reserved native relation fields",
			));
		}
		let table = intent.relation.class().to_owned();
		let relation = conversion::record_out(intent.relation, context, 0)?;
		let source = conversion::record_out(intent.source, context, 0)?;
		let target = conversion::record_out(intent.target, context, 0)?;
		let properties = conversion::outgoing(Value::Object(intent.properties), context, 0)?;
		let mut guard =
			RemoteTransaction(Some(self.client.clone().begin().await.map_err(external)?));
		let transaction = guard.0.as_ref().ok_or_else(|| {
			FerrosGrynnError::bridge_operation_error("external transaction is absent")
		})?;
		let mut response = transaction
			.query("SELECT * FROM ONLY $relation")
			.bind(("relation", relation.clone()))
			.await
			.map_err(external)?;
		if response.num_statements() != 1 {
			return Err(FerrosGrynnError::bridge_operation_error(
				"mirror lookup result count is invalid",
			));
		}
		let existing = match response.take::<sdk::Value>(0) {
			Ok(value) => value,
			Err(error) if matches!(error.not_found_details(), Some(sdk::NotFoundError::Table { name }) if name == &table) => {
				sdk::Value::None
			}
			Err(error) => return Err(external(error)),
		};
		let value = if matches!(existing, sdk::Value::None | sdk::Value::Null) {
			context.check()?;
			let response = transaction
				.query("RELATE $from->$relation->$to CONTENT $properties RETURN AFTER")
				.bind(sdk::Object::from(std::collections::BTreeMap::from([
					("from".to_owned(), sdk::Value::RecordId(source.clone())),
					("relation".to_owned(), sdk::Value::RecordId(relation.clone())),
					("to".to_owned(), sdk::Value::RecordId(target.clone())),
					("properties".to_owned(), properties.clone()),
				])))
				.await
				.map_err(external)?;
			let value = single(response)?;
			match value {
				sdk::Value::Array(mut rows) if rows.len() == 1 => rows.remove(0),
				value => value,
			}
		} else {
			existing
		};
		verify_relation(&value, &relation, &source, &target, &properties)?;
		context.check()?;
		let transaction = guard.0.take().ok_or_else(|| {
			FerrosGrynnError::bridge_operation_error("external transaction is absent")
		})?;
		transaction.commit().await.map_err(external)?;
		Ok(value)
	}
}
impl<C: Connection> Bridge for SurrealBridge<C> {
	/// Returns database for `SurrealBridge<C>`.
	fn database(&self) -> Option<BridgeDatabaseRef> {
		Some(self.database.reference())
	}

	/// Returns capabilities for `SurrealBridge<C>`.
	fn capabilities(&self) -> BridgeCapabilities {
		BridgeCapabilities::FULL
	}

	/// Dispatches queries, record reads or relation mirroring through the
	/// configured SurrealDB client.
	fn execute(&self, operation: BridgeOperation, context: BridgeContext) -> BridgeFuture<'_> {
		Box::pin(async move { self.operation(operation, &context).await })
	}
}

/// Dropping an interrupted operation schedules rollback of its open
/// transaction.
struct RemoteTransaction<C: Connection>(
	/// Dropping an interrupted operation schedules rollback of its open
	/// transaction.
	Option<surrealdb::method::Transaction<C>>,
);
impl<C: Connection> Drop for RemoteTransaction<C> {
	/// Runs cleanup for this `RemoteTransaction<C>` before its fields are
	/// released.
	fn drop(&mut self) {
		if let Some(transaction) = self.0.take()
			&& let Ok(runtime) = tokio::runtime::Handle::try_current()
		{
			let _rollback = runtime.spawn(async move {
				let _result = transaction.cancel().await;
			});
		}
	}
}
/// Rejects conflicting native identity, incidence and supplied property fields.
fn verify_relation(
	value: &sdk::Value,
	relation: &sdk::RecordId,
	source: &sdk::RecordId,
	target: &sdk::RecordId,
	properties: &sdk::Value,
) -> FerrosGrynnResult<()> {
	let sdk::Value::Object(row) = value else {
		return Err(FerrosGrynnError::bridge_mapping_error("external relation is not an object"));
	};
	let sdk::Value::Object(properties) = properties else {
		return Err(FerrosGrynnError::bridge_operation_error(
			"mirror properties are not an object",
		));
	};
	if row.get("id") != Some(&sdk::Value::RecordId(relation.clone()))
		|| row.get("in") != Some(&sdk::Value::RecordId(source.clone()))
		|| row.get("out") != Some(&sdk::Value::RecordId(target.clone()))
		|| properties
			.iter()
			.any(|(name, value)| row.get(name).unwrap_or(&sdk::Value::None) != value)
	{
		return Err(FerrosGrynnError::bridge_mapping_error(
			"external identity contains a conflicting relation",
		));
	}
	Ok(())
}

/// Own extension implemented on the original SDK query builder.
pub trait SurrealQueryBridgeExt<'q, C: Connection> {
	/// Adds a read-only FG source while preserving the native query and
	/// bindings.
	fn use_bridge<B: Bridge + 'static>(
		self,
		adapter: B,
		data: BridgeData<'_>,
	) -> SurrealBridgedQuery<'q, C>;
}
impl<'q, C: Connection> SurrealQueryBridgeExt<'q, C> for Query<'q, C> {
	/// Wraps this native query and registers its first read-only Ferros-Grynn
	/// source.
	fn use_bridge<B: Bridge + 'static>(
		self,
		adapter: B,
		data: BridgeData<'_>,
	) -> SurrealBridgedQuery<'q, C> {
		SurrealBridgedQuery {
			query: self,
			sources: Vec::new(),
			options: QueryExecutionOptions::default(),
			error: None,
		}
		.use_bridge(adapter, data)
	}
}
/// Awaitable native SDK query with FG source orchestration.
#[must_use = "queries do nothing until awaited"]
pub struct SurrealBridgedQuery<'q, C: Connection> {
	/// Original native builder, including every ordinary binding and statement.
	query: Query<'q, C>,
	/// Read-only FG source operations executed before native receiving IO.
	sources: Vec<super::query::SourceBinding>,
	/// Shared source interruption and conversion limits.
	options: QueryExecutionOptions,
	/// Deferred builder error returned before either side executes.
	error: Option<FerrosGrynnError>,
}
impl<'q, C: Connection> SurrealBridgedQuery<'q, C> {
	/// Preserves ordinary native binding behavior; bridge bindings are applied
	/// last.
	pub fn bind<VariablesArg: surrealdb::method::IntoVariables>(
		mut self,
		variables: VariablesArg,
	) -> Self {
		self.query = self.query.bind(variables);
		self
	}

	/// Appends native statements through the original query builder.
	pub fn query<SourceArg: Into<std::borrow::Cow<'q, str>>>(mut self, source: SourceArg) -> Self {
		self.query = self.query.query(source);
		self
	}

	/// Supplies FG source and shared conversion budgets.
	pub fn options(mut self, options: QueryExecutionOptions) -> Self {
		self.options = options;
		self
	}

	/// Adds another InternalQuery source and rejects external sources.
	pub fn use_bridge<B: Bridge + 'static>(mut self, adapter: B, data: BridgeData<'_>) -> Self {
		match super::query::SourceBinding::new(Arc::new(adapter), data) {
			Ok(source) if source.internal && source.adapter.capabilities().internal_queries() => {
				self.sources.push(source);
			}
			Ok(_) => {
				self.error = Some(FerrosGrynnError::bridge_operation_error(
					"native receiving query requires a read-only InternalQuery adapter",
				));
			}
			Err(error) => self.error = Some(error),
		};
		self
	}

	/// Executes source preflight and the retained native receiving query.
	/// # Errors
	/// Returns FG source, lossless conversion or native receiving errors.
	pub async fn execute(mut self) -> FerrosGrynnResult<IndexedResults> {
		if let Some(error) = self.error {
			return Err(error);
		}
		self.options.validate()?;
		let mut targets = BTreeSet::new();
		for source in &self.sources {
			for target in source.targets() {
				if !targets.insert(target.to_owned()) {
					return Err(FerrosGrynnError::bridge_operation_error(
						"two bridge sources bind the same parameter",
					));
				}
			}
		}
		let cancellation = crate::gql::QueryCancellation::default();
		let _cancel = super::query::CancelOnDrop(cancellation.clone());
		let mut context = BridgeContext {
			options: self.options.clone(),
			cancellation: cancellation.clone(),
			parent_cancellation: self.options.cancellation.clone(),
			deadline: self
				.options
				.statement_timeout
				.and_then(|timeout| std::time::Instant::now().checked_add(timeout)),
		};
		context.options.cancellation = Some(cancellation);
		context.options.context.bridge_memory = Some(Arc::new(parking_lot::Mutex::new(
			crate::gql::BarrierAccount::new(context.options.barrier_memory_bytes.get()),
		)));
		let mut bindings = Object::new();
		for source in self.sources {
			source.bind(&mut bindings, &context).await?;
		}
		self.query = self.query.bind(conversion::outgoing(Value::Object(bindings), &context, 0)?);
		context.check()?;
		let mut future = self.query.into_future();
		loop {
			context.check()?;
			if let Ok(result) =
				tokio::time::timeout(std::time::Duration::from_millis(25), &mut future).await
			{
				return result.map_err(external)?.check().map_err(external);
			}
		}
	}
}
impl<'q, C: Connection> IntoFuture for SurrealBridgedQuery<'q, C> {
	type IntoFuture = Pin<Box<dyn std::future::Future<Output = Self::Output> + Send + 'q>>;
	type Output = FerrosGrynnResult<IndexedResults>;

	/// Converts this value into future for `SurrealBridgedQuery<'q, C>`.
	fn into_future(self) -> Self::IntoFuture {
		Box::pin(self.execute())
	}
}

//! Source bindings and one bounded synchronous/async bridge dispatcher.

use std::{
	collections::{BTreeMap, BTreeSet},
	sync::{Arc, mpsc},
	time::{Duration, Instant},
};

use parking_lot::Mutex;

use super::{
	Bridge,
	BridgeCapabilities,
	BridgeContext,
	BridgeData,
	BridgeDatabase,
	BridgeDatabaseId,
	BridgeDatabaseOptions,
	BridgeDatabaseRef,
	BridgeMappingRef,
	BridgeMappingState,
	BridgeOperation,
};
use crate::{
	datastore::Datastore,
	gql::{QueryCancellation, QueryExecutionOptions, QueryResult},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{Object, Text, Value},
};

/// A durable bridge descriptor paired with its query-scoped runtime adapter.
type BoundBridgeAdapter = (Arc<BridgeDatabase>, Arc<dyn Bridge>);

/// One independent external effect; local query success is retained separately.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct BridgeEffectOutcome {
	/// Stable operation identity for explicit retry/resume.
	pub mapping: BridgeMappingRef,
	/// Confirmation state or the observed independent external failure.
	pub result: FerrosGrynnResult<BridgeMappingState>,
}

/// Owned local results and external outcomes after the ordinary commit
/// barriers.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct BridgeQueryResult {
	/// Ordinary local query results, including committed local success.
	pub results: Vec<QueryResult>,
	/// Independent external outcomes in commit order.
	pub effects: Vec<BridgeEffectOutcome>,
}

/// Prepared receiving query; no operation executes before execute is polled.
#[must_use = "query builders do nothing until execute is awaited"]
pub struct BridgeQuery {
	/// Datastore retained for the lifetime of this operation.
	store: Datastore,
	/// Owned source input evaluated by the selected adapter.
	source: Text,
	/// Typed input parameters retained until execution.
	variables: Object,
	/// Caller-owned execution and resource limits.
	options: QueryExecutionOptions,
	/// Ordered bridge sources applied after ordinary bindings.
	sources: Vec<SourceBinding>,
	/// Deferred builder validation error, returned before any IO.
	error: Option<FerrosGrynnError>,
}

/// Ferros-Grynn counterpart of the native SDK query extension.
pub trait FerrosQueryBridgeExt: Sized {
	/// Adds one external source while retaining receiving query state.
	#[must_use]
	fn use_bridge<B: Bridge + 'static>(self, adapter: B, data: BridgeData<'_>) -> Self;
}
impl FerrosQueryBridgeExt for BridgeQuery {
	/// Returns use bridge for `BridgeQuery`.
	fn use_bridge<B: Bridge + 'static>(self, adapter: B, data: BridgeData<'_>) -> Self {
		BridgeQuery::use_bridge(self, adapter, data)
	}
}

/// Owned source description retained without borrowing a query builder.
pub struct SourceBinding {
	/// Runtime-only client implementing the object-safe bridge port.
	pub(crate) adapter: Arc<dyn Bridge>,
	/// Owned operation submitted to the selected adapter.
	pub(crate) operation: BridgeOperation,
	/// Explicit source-field to receiving-parameter correspondence.
	pub(crate) mapping: Option<BTreeMap<Text, Text>>,
	/// Parameter name for a record/class source.
	pub(crate) target: Option<Text>,
	/// Whether the source requires the read-only GQL effect ceiling.
	pub(crate) internal: bool,
}

impl SourceBinding {
	/// Constructs the owned state used by this bounded operation.
	pub(crate) fn new(adapter: Arc<dyn Bridge>, data: BridgeData<'_>) -> FerrosGrynnResult<Self> {
		let (operation, mapping, target, internal) = match data {
			BridgeData::RecordOrClass(source, target) => {
				(BridgeOperation::RecordOrClass(source.into()), None, Some(target.into()), false)
			}
			BridgeData::ExternalQuery(source, mapping) => (
				BridgeOperation::Query {
					source: source.into(),
					variables: Object::new(),
				},
				mapping,
				None,
				false,
			),
			BridgeData::InternalQuery(source, mapping) => (
				BridgeOperation::Query {
					source: source.into(),
					variables: Object::new(),
				},
				mapping,
				None,
				true,
			),
		};
		let mapping = mapping.map(|items| {
			items
				.into_iter()
				.map(|(source, target)| (Text::from(source), Text::from(target)))
				.collect::<BTreeMap<_, _>>()
		});
		let value = Self {
			adapter,
			operation,
			mapping,
			target,
			internal,
		};
		let mut names = BTreeSet::new();
		for name in value.targets() {
			if name.is_empty()
				|| name.len() > 4096
				|| name.contains(['\0', '$'])
				|| !names.insert(name)
			{
				return Err(FerrosGrynnError::bridge_operation_error(
					"invalid or duplicate bridge parameter name",
				));
			}
		}
		Ok(value)
	}

	/// Lists the receiving parameter names for collision preflight.
	pub(crate) fn targets(&self) -> Vec<&str> {
		if let Some(target) = &self.target {
			vec![target.as_str()]
		} else if let Some(mapping) = &self.mapping {
			mapping.values().map(Text::as_str).collect()
		} else {
			vec!["bridge"]
		}
	}

	/// Applies exactly one owned source result without reshaping values.
	pub(crate) async fn bind(
		self,
		variables: &mut Object,
		context: &BridgeContext,
	) -> FerrosGrynnResult<()> {
		let value = execute_adapter(&self.adapter, self.operation, context.clone()).await?;
		context.admit_value(&value)?;
		if let Some(target) = self.target {
			variables.insert(target, value);
		} else if let Some(mapping) = self.mapping {
			let Value::Object(mut object) = value else {
				return Err(FerrosGrynnError::bridge_operation_error(
					"mapped bridge source must return Object",
				));
			};
			for (source, target) in mapping {
				let value = object.remove(source.as_str()).ok_or_else(|| {
					FerrosGrynnError::bridge_operation_error("mapped source field is absent")
				})?;
				variables.insert(target, value);
			}
		} else {
			variables.insert("bridge", value);
		}
		Ok(())
	}
}

/// Cancelling an orchestration future also interrupts its blocking query
/// worker.
pub struct CancelOnDrop(
	/// Cancelling an orchestration future also interrupts its blocking query
	/// worker.
	pub(crate) QueryCancellation,
);
impl Drop for CancelOnDrop {
	/// Runs cleanup for this `CancelOnDrop` before its fields are released.
	fn drop(&mut self) {
		self.0.cancel();
	}
}

impl BridgeQuery {
	/// Constructs the owned state used by this bounded operation.
	pub(crate) fn new(store: Datastore, source: &str) -> Self {
		Self {
			store,
			source: source.into(),
			variables: Object::new(),
			options: QueryExecutionOptions::default(),
			sources: Vec::new(),
			error: None,
		}
	}

	/// Supplies receiving parameters; bridge parameters are applied afterwards.
	#[must_use = "the updated query builder must be executed"]
	pub fn bind(mut self, variables: Object) -> Self {
		for (name, value) in variables {
			self.variables.insert(name, value);
		}
		self
	}

	/// Supplies parser, cancellation, deadline and bounded-resource controls.
	#[must_use = "the updated query builder must be executed"]
	pub fn options(mut self, options: QueryExecutionOptions) -> Self {
		self.options = options;
		self
	}

	/// Adds one query-scoped external source without importing atoms or
	/// classes.
	#[must_use = "the updated query builder must be executed"]
	pub fn use_bridge<B: Bridge + 'static>(mut self, adapter: B, data: BridgeData<'_>) -> Self {
		match SourceBinding::new(Arc::new(adapter), data) {
			Ok(binding) if !binding.internal => self.sources.push(binding),
			Ok(_) => {
				self.error = Some(FerrosGrynnError::bridge_operation_error(
					"Ferros-Grynn receiving query requires ExternalQuery",
				));
			}
			Err(error) => self.error = Some(error),
		}
		self
	}

	/// Executes source bindings, the existing GQL pipeline and explicit
	/// mirrors.
	///
	/// # Errors
	/// Returns preflight/source/worker errors. Committed local results retain
	/// separate external outcomes, including Pending references on failure.
	pub async fn execute(mut self) -> FerrosGrynnResult<BridgeQueryResult> {
		if let Some(error) = self.error {
			return Err(error);
		}
		self.options.validate()?;
		crate::gql::parse(&self.source, self.options.parser_settings)?;
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
		let cancellation = QueryCancellation::default();
		let _cancel = CancelOnDrop(cancellation.clone());
		let memory = Arc::new(Mutex::new(crate::gql::BarrierAccount::new(
			self.options.barrier_memory_bytes.get(),
		)));
		let context = BridgeContext {
			options: self.options.clone(),
			cancellation: cancellation.clone(),
			parent_cancellation: self.options.cancellation.clone(),
			deadline: self
				.options
				.statement_timeout
				.and_then(|timeout| Instant::now().checked_add(timeout)),
		};
		let mut context = context;
		context.options.cancellation = Some(cancellation);
		context.options.context.bridge_memory = Some(memory);
		let (scope, receiver) = BridgeQueryScope::new(&self.store, context.clone())?;
		for source in self.sources.drain(..) {
			scope.attach_source(&self.store, &source.adapter)?;
			source.bind(&mut self.variables, &context).await?;
		}
		let binding_bytes = self
			.variables
			.iter()
			.try_fold(std::mem::size_of::<Object>(), |bytes, (name, value)| {
				bytes
					.checked_add(name.len())
					.and_then(|bytes| bytes.checked_add(std::mem::size_of::<(Text, Value)>()))
					.and_then(|bytes| bytes.checked_add(value.estimated_heap_bytes()))
			})
			.ok_or_else(|| FerrosGrynnError::operation_overflow("bridge parameter bytes"))?;
		context.admit_bytes(binding_bytes)?;
		self.options = context.options.clone();
		self.options.context.bridge_scope = Some(scope.clone());
		let store = self.store.clone();
		let source = self.source;
		let variables = self.variables;
		let options = self.options;
		let runtime = tokio::runtime::Handle::try_current().map_err(|error| {
			FerrosGrynnError::bridge_operation_error(
				"bridge orchestration requires a Tokio runtime with time enabled",
			)
			.with_technical_source(error)
		})?;
		let worker = runtime
			.spawn_blocking(move || store.execute_with_options(&source, &variables, options));
		let results = run_worker(&scope, receiver, worker).await?;
		let mappings = scope.pending.lock().iter().copied().collect::<Vec<_>>();
		for mapping in mappings {
			let Some(row) = self.store.begin().bridge_mapping(mapping)? else {
				continue;
			};
			if row.state == BridgeMappingState::Tombstoned {
				continue;
			}
			let result =
				self.store.resume_bridge_mapping_with_context(mapping, &scope, &context).await;
			scope.effect(BridgeEffectOutcome {
				mapping,
				result,
			});
		}
		let effects = scope.effects.lock().clone();
		Ok(BridgeQueryResult {
			results,
			effects,
		})
	}
}

/// Both directions share one dispatcher and the existing synchronous executor.
async fn run_worker(
	scope: &Arc<BridgeQueryScope>,
	mut receiver: tokio::sync::mpsc::Receiver<Request>,
	mut worker: tokio::task::JoinHandle<FerrosGrynnResult<Vec<QueryResult>>>,
) -> FerrosGrynnResult<Vec<QueryResult>> {
	let dispatch = async {
		while let Some(request) = receiver.recv().await {
			match request {
				Request::Operation {
					database,
					operation,
					context,
					reply,
				} => {
					let result = match scope.adapter(database) {
						Ok(adapter) => execute_adapter(&adapter, operation, context).await,
						Err(error) => Err(error),
					};
					let _delivered = reply.send(result);
				}
			}
		}
	};
	let mut dispatch = Box::pin(dispatch);
	std::future::poll_fn(|cx| {
		let _progress = std::future::Future::poll(dispatch.as_mut(), cx);
		std::pin::Pin::new(&mut worker).poll(cx)
	})
	.await
	.map_err(|error| {
		FerrosGrynnError::bridge_operation_error("GQL bridge worker failed")
			.with_technical_source(error)
	})?
}

/// Source-side queries retain read-only access even through nested bridge
/// FETCH.
pub async fn internal_query(
	store: Datastore,
	source: Text,
	variables: Object,
	mut context: BridgeContext,
) -> FerrosGrynnResult<Value> {
	context.check()?;
	context.options.context.bridge_scope = None;
	context.options.context.bridge_read_only = true;
	let program = crate::gql::parse(&source, context.options.parser_settings)?;
	if program.statements.len() != 1 {
		return Err(FerrosGrynnError::bridge_operation_error(
			"InternalQuery requires one resulting statement",
		));
	}
	let (scope, receiver) = BridgeQueryScope::new(&store, context.clone())?;
	let mut options = context.options();
	options.context.bridge_scope = Some(scope.clone());
	let runtime = tokio::runtime::Handle::try_current().map_err(|error| {
		FerrosGrynnError::bridge_operation_error(
			"InternalQuery requires a Tokio runtime with time enabled",
		)
		.with_technical_source(error)
	})?;
	let worker = runtime.spawn_blocking(move || {
		let mut tx = store.begin_with(crate::datastore::TransactionOptions::new(
			crate::datastore::IsolationLevel::Snapshot,
			crate::datastore::TransactionAccess::ReadOnly,
		));
		tx.execute_with_options(&source, &variables, options)
	});
	let mut results = run_worker(&scope, receiver, worker).await?;
	if results.len() != 1 {
		return Err(FerrosGrynnError::bridge_operation_error(
			"InternalQuery did not return one result",
		));
	}
	Ok(results.remove(0).result?.into_parts().0)
}

/// Owned request; page guards and transactions never enter the async
/// dispatcher.
enum Request {
	/// One owned native IO request and bounded response channel.
	Operation {
		/// Registered database descriptor or identity for this operation.
		database: BridgeDatabaseId,
		/// Owned operation submitted to the selected adapter.
		operation: BridgeOperation,
		/// Shared interruption authority and boundary memory account.
		context: BridgeContext,
		/// Bounded one-shot response channel to the waiting synchronous worker.
		reply: mpsc::SyncSender<FerrosGrynnResult<Value>>,
	},
}

/// Query-only dispatcher, cache and commit outcomes shared with the worker.
pub struct BridgeQueryScope {
	/// Datastore retained for the lifetime of this operation.
	store: Datastore,
	/// Observed independent external outcomes in local commit order.
	effects: Mutex<Vec<BridgeEffectOutcome>>,
	/// Shared interruption authority and boundary memory account.
	pub(crate) context: BridgeContext,
	/// Query-scoped runtime clients paired with their durable descriptors.
	adapters: Mutex<BTreeMap<BridgeDatabaseId, BoundBridgeAdapter>>,
	/// Bounded async request queue; no page guard crosses this port.
	requests: tokio::sync::mpsc::Sender<Request>,
	/// Reserved mapping identities awaiting the local commit barrier.
	pub(crate) pending: Mutex<Vec<BridgeMappingRef>>,
	/// Exact external record responses reused within this query.
	cache: Mutex<BTreeMap<(BridgeDatabaseId, crate::values::RecordId), Arc<Value>>>,
}
impl std::fmt::Debug for BridgeQueryScope {
	/// Formats `BridgeQueryScope` using the requested formatter.
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("BridgeQueryScope")
			.field("adapters", &self.adapters.lock().len())
			.finish_non_exhaustive()
	}
}

impl BridgeQueryScope {
	/// Constructs the owned state used by this bounded operation.
	fn new(
		store: &Datastore,
		context: BridgeContext,
	) -> FerrosGrynnResult<(Arc<Self>, tokio::sync::mpsc::Receiver<Request>)> {
		let root = store.inner.current_root();
		let mut adapters = BTreeMap::new();
		for (id, adapter) in store.inner.bridge_adapters.read().iter() {
			let database = root.bridges.databases.get(id).ok_or_else(|| {
				FerrosGrynnError::bridge_mapping_error("runtime bridge has no durable identity")
			})?;
			adapters.insert(*id, (database.clone(), adapter.clone()));
		}
		let (sender, receiver) = tokio::sync::mpsc::channel(1);
		Ok((
			Arc::new(Self {
				store: store.clone(),
				effects: Mutex::new(Vec::new()),
				context,
				adapters: Mutex::new(adapters),
				requests: sender,
				pending: Mutex::new(Vec::new()),
				cache: Mutex::new(BTreeMap::new()),
			}),
			receiver,
		))
	}

	/// Retains the external outcome separately from local query results.
	fn effect(&self, outcome: BridgeEffectOutcome) {
		self.effects.lock().push(outcome);
	}

	/// Called only after the local commit lock and transaction have been
	/// released.
	pub(crate) fn committed(&self) {
		let pending = std::mem::take(&mut *self.pending.lock());
		let mappings = pending
			.into_iter()
			.filter(|reference| {
				self.store
					.inner
					.current_root()
					.bridges
					.get(reference.id)
					.is_some_and(|row| row.state != BridgeMappingState::Tombstoned)
			})
			.collect::<Vec<_>>();
		for mapping in mappings {
			let result = (|| {
				let (tx, operation) = self.store.prepare_bridge_mirror(mapping, &self.context)?;
				if let Some((database, operation)) = operation {
					self.call_context(database, operation, self.context.clone(), &|| {
						self.context.check()
					})?;
					self.store.confirm_bridge_mirror(tx, mapping, &self.context)?;
				}
				Ok(BridgeMappingState::Ready)
			})();
			self.effect(BridgeEffectOutcome {
				mapping,
				result,
			});
		}
	}

	/// Discards reservations staged by an aborted local transaction.
	pub(crate) fn cancelled(&self) {
		self.pending.lock().clear();
	}

	/// Adds a query-only client after checking its datastore origin.
	fn attach_source(&self, store: &Datastore, adapter: &Arc<dyn Bridge>) -> FerrosGrynnResult<()> {
		if let Some(reference) = adapter.database() {
			let root = store.inner.current_root();
			if reference.datastore != root.database_uuid {
				return Err(FerrosGrynnError::bridge_mapping_error(
					"query adapter belongs to another datastore",
				));
			}
			let database = root.bridges.databases.get(&reference.id).ok_or_else(|| {
				FerrosGrynnError::bridge_mapping_error("query adapter database is absent")
			})?;
			self.adapters.lock().insert(reference.id, (database.clone(), adapter.clone()));
		}
		Ok(())
	}

	/// Runtime-only client implementing the object-safe bridge port.
	fn adapter(&self, database: BridgeDatabaseId) -> FerrosGrynnResult<Arc<dyn Bridge>> {
		self.adapters.lock().get(&database).map(|(_, adapter)| adapter.clone()).ok_or_else(|| {
			FerrosGrynnError::bridge_operation_error("named runtime adapter is not attached")
		})
	}

	/// Resolves the named runtime adapter against its durable descriptor.
	pub(crate) fn resolve(
		&self,
		alias: &str,
	) -> FerrosGrynnResult<(Arc<BridgeDatabase>, BridgeCapabilities)> {
		self.adapters
			.lock()
			.values()
			.find(|(database, _)| database.alias == alias)
			.map(|(database, adapter)| (database.clone(), adapter.capabilities()))
			.ok_or_else(|| {
				FerrosGrynnError::bridge_operation_error("named runtime adapter is not attached")
			})
	}

	/// Submits IO and waits under the existing statement interruption controls.
	pub(crate) fn call(
		&self,
		database: BridgeDatabaseId,
		operation: BridgeOperation,
		control: &crate::gql::ExecutionControl,
	) -> FerrosGrynnResult<Value> {
		control.check()?;
		let mut context = self.context.clone();
		context.deadline = match (context.deadline, control.deadline()) {
			(Some(left), Some(right)) => Some(left.min(right)),
			(Some(value), None) | (None, Some(value)) => Some(value),
			(None, None) => None,
		};
		self.call_context(database, operation, context, &|| control.check())
	}

	/// Waits on async IO without scheduling another blocking-pool worker.
	fn call_context(
		&self,
		database: BridgeDatabaseId,
		operation: BridgeOperation,
		context: BridgeContext,
		check: &impl Fn() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Value> {
		check()?;
		let (sender, receiver) = mpsc::sync_channel(1);
		self.requests
			.blocking_send(Request::Operation {
				database,
				operation,
				context,
				reply: sender,
			})
			.map_err(|_error| FerrosGrynnError::query_cancelled())?;
		loop {
			check()?;
			// Periodic wakeups let the synchronous caller observe statement
			// cancellation and deadlines.
			match receiver.recv_timeout(Duration::from_millis(25)) {
				Ok(result) => return result,
				Err(mpsc::RecvTimeoutError::Timeout) => {}
				Err(mpsc::RecvTimeoutError::Disconnected) => {
					return Err(FerrosGrynnError::query_cancelled());
				}
			}
		}
	}

	/// Returns the admitted query-cached native record response.
	pub(crate) fn records(
		&self,
		database: BridgeDatabaseId,
		record: &crate::values::RecordId,
		control: &crate::gql::ExecutionControl,
	) -> FerrosGrynnResult<Arc<Value>> {
		let key = (database, record.clone());
		let cached = self.cache.lock().get(&key).cloned();
		if let Some(value) = cached {
			return Ok(value);
		}
		self.prefetch(database, vec![record.clone()], control)?;
		self.cache.lock().get(&key).cloned().ok_or_else(|| {
			FerrosGrynnError::bridge_operation_error("record cache lost an admitted response")
		})
	}

	/// Borrow-only provenance check for repeated FETCH namespaces in this
	/// query.
	pub(crate) fn cached_record(
		&self,
		database: BridgeDatabaseId,
		record: &crate::values::RecordId,
	) -> Option<Arc<Value>> {
		self.cache.lock().get(&(database, record.clone())).cloned()
	}

	/// Deduplicates exact native identities and admits one bounded batch.
	pub(crate) fn prefetch(
		&self,
		database: BridgeDatabaseId,
		records: Vec<crate::values::RecordId>,
		control: &crate::gql::ExecutionControl,
	) -> FerrosGrynnResult<()> {
		let bytes = records
			.iter()
			.try_fold(0_usize, |bytes, record| {
				bytes.checked_add(record.estimated_heap_bytes().checked_mul(4)?.checked_add(512)?)
			})
			.ok_or_else(|| FerrosGrynnError::operation_overflow("bridge batch identities"))?;
		self.context.admit_bytes(bytes)?;
		let records = records
			.into_iter()
			.filter(|record| !self.cache.lock().contains_key(&(database, record.clone())))
			.collect::<BTreeSet<_>>()
			.into_iter()
			.collect::<Vec<_>>();
		if records.is_empty() {
			return Ok(());
		}
		let result = self.call(database, BridgeOperation::Records(records.clone()), control)?;
		let Value::Array(rows) = result else {
			return Err(FerrosGrynnError::bridge_operation_error(
				"record batch response is not an array",
			));
		};
		if rows.len() != records.len() {
			return Err(FerrosGrynnError::bridge_operation_error(
				"record batch cardinality mismatch",
			));
		}
		for (record, value) in records.into_iter().zip(rows) {
			self.context.admit_value(&value)?;
			self.cache.lock().insert((database, record), Arc::new(value));
		}
		Ok(())
	}
}

/// Checks capabilities and polls adapter IO under the original deadline.
pub async fn execute_adapter(
	adapter: &Arc<dyn Bridge>,
	operation: BridgeOperation,
	context: BridgeContext,
) -> FerrosGrynnResult<Value> {
	let capabilities = adapter.capabilities();
	let supported = match operation {
		BridgeOperation::Query {
			..
		}
		| BridgeOperation::RecordOrClass(_) => capabilities.queries(),
		BridgeOperation::Records(_) => capabilities.records(),
		BridgeOperation::Mirror(_) => capabilities.mirror(),
	};
	if !supported {
		return Err(FerrosGrynnError::bridge_operation_error(
			"adapter does not support the requested capability",
		));
	}
	let mut future = adapter.execute(operation, context.clone());
	loop {
		context.check()?;
		if let Ok(result) = tokio::time::timeout(Duration::from_millis(25), &mut future).await {
			return result;
		}
	}
}

impl Datastore {
	/// Creates explicit asynchronous bridge orchestration over the sync
	/// executor.
	#[must_use = "the query builder must be executed"]
	pub fn query(&self, source: &str) -> BridgeQuery {
		BridgeQuery::new(self.clone(), source)
	}

	/// Registers non-secret database metadata through the ordinary transaction.
	/// # Errors
	/// Returns validation, FCW/SSI, admission or persistence errors.
	pub fn register_bridge_database(
		&self,
		options: BridgeDatabaseOptions,
	) -> FerrosGrynnResult<BridgeDatabaseRef> {
		let mut tx = self.begin();
		let reference = tx.register_bridge_database(options)?;
		tx.commit()?;
		Ok(reference)
	}

	/// Returns durable database metadata for runtime client construction.
	/// # Errors
	/// Returns ordinary read transaction errors.
	pub fn bridge_database(&self, alias: &str) -> FerrosGrynnResult<Option<BridgeDatabase>> {
		self.begin().bridge_database(alias)
	}

	/// Attaches a client to its exact durable descriptor without persisting it.
	/// # Errors
	/// Rejects absent/foreign identities and adapters configured for another
	/// database.
	pub fn attach_bridge<B: Bridge + 'static>(
		&self,
		reference: BridgeDatabaseRef,
		adapter: B,
	) -> FerrosGrynnResult<()> {
		let root = self.inner.current_root();
		if root.database_uuid != reference.datastore
			|| !root.bridges.databases.contains_key(&reference.id)
			|| adapter.database() != Some(reference)
		{
			return Err(FerrosGrynnError::bridge_mapping_error(
				"runtime adapter database identity mismatch",
			));
		}
		self.inner.bridge_adapters.write().insert(reference.id, Arc::new(adapter));
		Ok(())
	}

	/// Explicitly verifies or creates one Pending external operation.
	/// # Errors
	/// Returns lookup, external verification, cancellation or confirmation
	/// errors.
	pub async fn resume_bridge_mapping(
		&self,
		reference: BridgeMappingRef,
	) -> FerrosGrynnResult<BridgeMappingState> {
		self.resume_bridge_mapping_with_options(reference, QueryExecutionOptions::default()).await
	}

	/// Resumes with the same explicit deadline, cancellation and value budget.
	/// # Errors
	/// Returns operation, resource, interruption or confirmation errors.
	pub async fn resume_bridge_mapping_with_options(
		&self,
		reference: BridgeMappingRef,
		options: QueryExecutionOptions,
	) -> FerrosGrynnResult<BridgeMappingState> {
		options.validate()?;
		let cancellation = QueryCancellation::default();
		let _cancel = CancelOnDrop(cancellation.clone());
		let mut context = BridgeContext {
			options: options.clone(),
			cancellation: cancellation.clone(),
			parent_cancellation: options.cancellation.clone(),
			deadline: options
				.statement_timeout
				.and_then(|timeout| Instant::now().checked_add(timeout)),
		};
		context.options.cancellation = Some(cancellation);
		context.options.context.bridge_memory = Some(Arc::new(Mutex::new(
			crate::gql::BarrierAccount::new(options.barrier_memory_bytes.get()),
		)));
		let (scope, _receiver) = BridgeQueryScope::new(self, context.clone())?;
		self.resume_bridge_mapping_with_context(reference, &scope, &context).await
	}

	/// Verifies the saved external intent before local Ready confirmation.
	async fn resume_bridge_mapping_with_context(
		&self,
		reference: BridgeMappingRef,
		scope: &BridgeQueryScope,
		context: &BridgeContext,
	) -> FerrosGrynnResult<BridgeMappingState> {
		let (tx, operation) = self.prepare_bridge_mirror(reference, context)?;
		let Some((database, operation)) = operation else {
			return Ok(BridgeMappingState::Ready);
		};
		execute_adapter(&scope.adapter(database)?, operation, context.clone()).await?;
		context.check()?;
		let worker_context = context.clone();
		let store = self.clone();
		let runtime = tokio::runtime::Handle::try_current().map_err(|error| {
			FerrosGrynnError::bridge_operation_error(
				"mapping confirmation requires a Tokio runtime",
			)
			.with_technical_source(error)
		})?;
		let mut worker = runtime
			.spawn_blocking(move || store.confirm_bridge_mirror(tx, reference, &worker_context));
		loop {
			// Borrow the handle across timeouts: blocking confirmation must
			// finish before its result is returned.
			match tokio::time::timeout(Duration::from_millis(25), &mut worker).await {
				Ok(result) => {
					result.map_err(|error| {
						FerrosGrynnError::bridge_operation_error(
							"mapping confirmation worker failed",
						)
						.with_technical_source(error)
					})??;
					return Ok(BridgeMappingState::Ready);
				}
				Err(_) => {
					if context.check().is_err() {
						context.cancellation.cancel();
					}
				}
			}
		}
	}

	/// Captures local requirements before the independently committed mirror.
	fn prepare_bridge_mirror(
		&self,
		reference: BridgeMappingRef,
		context: &BridgeContext,
	) -> FerrosGrynnResult<(
		crate::datastore::Transaction,
		Option<(BridgeDatabaseId, BridgeOperation)>,
	)> {
		context.check()?;
		let mut tx = self.begin();
		let row = tx.prepare_bridge_resume(reference)?;
		if row.state == BridgeMappingState::Ready {
			return Ok((tx, None));
		}
		if row.state != BridgeMappingState::Pending {
			return Err(FerrosGrynnError::bridge_mapping_error(
				"tombstoned mapping cannot be resumed",
			));
		}
		let intent = row.mirror.ok_or_else(|| {
			FerrosGrynnError::bridge_mapping_error("Pending mapping has no operation")
		})?;
		context.admit_bytes(
			intent
				.properties
				.iter()
				.map(|(name, value)| name.len().saturating_add(value.estimated_heap_bytes()))
				.sum(),
		)?;
		Ok((tx, Some((row.external.database.id, BridgeOperation::Mirror(intent.as_ref().clone())))))
	}

	/// Publishes Ready on the existing blocking worker after async IO
	/// completes.
	fn confirm_bridge_mirror(
		&self,
		mut tx: crate::datastore::Transaction,
		reference: BridgeMappingRef,
		context: &BridgeContext,
	) -> FerrosGrynnResult<()> {
		let maintenance = match self.inner.options {
			crate::datastore::DatastoreOptions::Local(options) => options.maintenance,
			crate::datastore::DatastoreOptions::InMemory(options) => options.maintenance,
		};
		crate::storage::with_external_sort_control(
			maintenance.max_memory_bytes,
			maintenance.max_parallelism,
			context.cancellation.atomic(),
			|| {
				context.check()?;
				tx.confirm_bridge_mapping(reference)?;
				tx.commit_controlled(context.deadline, &|| context.check())?;
				Ok(())
			},
		)
	}
}

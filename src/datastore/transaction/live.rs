//! Transactional registration uses ordinary MVCC replay and savepoints.

use std::sync::Arc;

use super::{Mutation, Transaction};
use crate::{
	datastore::{ConflictKey, LiveDefinition, consts::SCHEMA_WRITE_BYTES},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	live::{LiveId, LiveInfo, LiveOptions, LiveQuery, LiveSource, LiveState, LiveVersion},
	storage::live_descriptor::{self, LiveDescriptor},
};

impl Transaction {
	/// Marks only successful text registrations for pre-publication session
	/// admission.
	pub(crate) fn note_gql_live_registration(&mut self, id: LiveId) -> FerrosGrynnResult<()> {
		self.ensure_capacity(std::mem::size_of::<LiveId>() * 2)?;
		self.gql_live_registrations.try_reserve(1).map_err(|error| {
			FerrosGrynnError::traversal_resource_limit_exceeded(
				"GQL LIVE registrations",
				self.gql_live_registrations.len() as u64 + 1,
			)
			.with_technical_source(error)
		})?;
		self.gql_live_registrations.push(id);
		self.write_bytes += std::mem::size_of::<LiveId>() * 2;
		Ok(())
	}

	/// Borrows the bounded IDs; no queue or snapshot is retained in this
	/// journal.
	pub(crate) fn gql_live_registrations(&self) -> &[LiveId] {
		&self.gql_live_registrations
	}

	/// Stages a subscription incarnation, activated only by successful commit.
	///
	/// # Errors
	///
	/// Returns invalid-source, nonrestorable-capture, import-DDL, read-only or
	/// resource errors. Rollback and savepoints discard the staged definition.
	#[doc = include_str!("../../../docs/rustdoc/datastore-transaction-live-example-01.md")]
	pub fn register_live(
		&mut self,
		query: LiveQuery,
		options: LiveOptions,
	) -> FerrosGrynnResult<LiveId> {
		self.run_gql_statement(|tx| tx.register_live_inner(query, options, None))
	}

	/// Text registration inherits its original deadline and cancellation.
	pub(crate) fn register_live_controlled(
		&mut self,
		query: LiveQuery,
		options: LiveOptions,
		control: &crate::gql::ExecutionControl,
	) -> FerrosGrynnResult<LiveId> {
		control.check()?;
		self.run_gql_statement(|tx| tx.register_live_inner(query, options, Some(control)))
	}

	/// Source resolution and ordinary analysis share one statement snapshot.
	fn register_live_inner(
		&mut self,
		query: LiveQuery,
		options: LiveOptions,
		parent: Option<&crate::gql::ExecutionControl>,
	) -> FerrosGrynnResult<LiveId> {
		self.prepare_schema_mutation()?;
		for source in query.sources() {
			self.validate_live_source(source)?;
		}
		let binding_options = crate::live::LiveBindingOptions::default()
			.with_budgets(options.scratch_bytes(), options.work())?;
		let dependencies = crate::datastore::query_dependencies(
			&query,
			&mut crate::datastore::BindingAdmission::new(&binding_options),
		)?;
		for dependency in dependencies {
			self.record_read(ConflictKey::LiveBinding(dependency));
		}
		let source_class = source_class(&self.working, query.source())?;
		if matches!(options.since(), crate::live::LiveSince::Cursor(_)) {
			return Err(FerrosGrynnError::invalid_argument(
				"an existing LIVE cursor requires resume_live, not a new registration",
			));
		}
		let recipe = LiveDescriptor::Query(query);
		let descriptor = live_descriptor::encode(&recipe, options.scratch_bytes())?;
		let mut cost = descriptor.len().checked_add(SCHEMA_WRITE_BYTES).ok_or_else(|| {
			FerrosGrynnError::invalid_argument("LIVE registration size overflows")
		})?;
		self.ensure_capacity(cost)?;
		if let LiveDescriptor::Query(query) = &recipe {
			crate::gql::live_lowering::validate_registration_controlled(
				self,
				&self.working.clone(),
				query,
				&options,
				parent,
			)?;
		}
		let LiveDescriptor::Query(query) = recipe else {
			return Err(FerrosGrynnError::invalid_argument("LIVE query descriptor expected"));
		};
		let root = Arc::new(self.working.clone());
		let bindings = |id| root.live_catalog().binding_value(id);
		let evaluation = crate::gql::live_lowering::evaluate_query_controlled(
			&root,
			&query,
			&bindings,
			&options,
			self.query_scratch_root(),
			&|key| self.record_read(key),
			None,
			&[],
			parent,
		)?;
		let mut validation = crate::datastore::BindingAdmission::new(&binding_options);
		super::live_result::admit_evaluation(&evaluation, &mut validation)?;
		super::live_result::validate_evaluation(&evaluation, &mut validation)?;
		let metadata_bytes = evaluation
			.functions
			.len()
			.checked_mul(8)
			.and_then(|bytes| bytes.checked_add(evaluation.record_sources.len().checked_mul(64)?))
			.and_then(|bytes| {
				bytes.checked_add(
					evaluation.capture_scope_name.as_ref().map_or(0, |name| name.len()),
				)
			})
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE registration captures"))?;
		let anchor_bytes =
			crate::datastore::LiveRhsScopeAnchor::retained_bytes(&evaluation.rhs_scope_anchors)?;
		cost = cost
			.checked_add(metadata_bytes)
			.and_then(|bytes| bytes.checked_add(anchor_bytes))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE registration bytes"))?;
		for (name, _) in &evaluation.record_sources {
			cost = cost
				.checked_add(name.len())
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE source names"))?;
		}
		self.ensure_capacity(cost)?;
		let id = LiveId::from_uuid(crate::values::Uuid::now_v7())?;
		let definition = Arc::new(LiveDefinition {
			id,
			version: LiveVersion::default(),
			state: LiveState::Pending,
			query: Arc::new(query),
			descriptor: descriptor.into(),
			source_class,
			capture_source: evaluation.capture_source,
			capture_scope_name: evaluation.capture_scope_name,
			rhs_scope_anchors: evaluation.rhs_scope_anchors,
			defined_at: 0,
			starting_after: match options.since() {
				crate::live::LiveSince::Versionstamp(stamp) => stamp.get(),
				_ => 0,
			},
			options,
			published_at: 0,
			acknowledged: 0,
			functions: evaluation.functions,
			record_sources: evaluation.record_sources,
			diagnostic: None,
		});
		Arc::make_mut(self.working.live_catalog_mut()).register(definition.clone())?;
		self.insert_write(ConflictKey::LiveDefinition(id), 0);
		self.write_bytes += cost;
		self.mark_range_change(ConflictKey::LiveCatalogRange);
		self.mutations.push(Mutation::RegisterLive(definition));
		Ok(id)
	}

	/// Stops one incarnation transactionally, preserving its identity.
	///
	/// # Errors
	///
	/// Returns missing-definition, read-only, import-item or resource errors.
	/// Standalone close commits remain available during a leased import.
	pub fn close_live(&mut self, id: LiveId) -> FerrosGrynnResult<()> {
		self.prepare_control()?;
		let definition =
			self.working.live_catalog().queries.get(&id).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("LIVE identity is not registered")
			})?;
		if definition.state == LiveState::Closed {
			return Ok(());
		}
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		Arc::make_mut(self.working.live_catalog_mut()).close(id, self.snapshot.commit_ts)?;
		self.insert_write(ConflictKey::LiveDefinition(id), 0);
		self.write_bytes += SCHEMA_WRITE_BYTES;
		self.mark_range_change(ConflictKey::LiveCatalogRange);
		self.mutations.push(Mutation::CloseLive(id));
		Ok(())
	}

	/// Stages only a token already checked against its issuing consumer.
	/// This private surface cannot be called through arbitrary SDK cursors.
	pub(in crate::datastore) fn acknowledge_live(
		&mut self,
		token: &crate::live::LiveCursor,
	) -> FerrosGrynnResult<()> {
		self.prepare_control()?;
		let id = token.live_id();
		let definition =
			self.working.live_catalog().queries.get(&id).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("LIVE identity is not registered")
			})?;
		token.validate(self.working.database_uuid.into(), id, definition.version)?;
		let stamp = token.versionstamp().get();
		if stamp > self.snapshot.commit_ts {
			return Err(FerrosGrynnError::invalid_argument("LIVE ACK exceeds the captured commit"));
		}
		if stamp <= definition.acknowledged {
			return Ok(());
		}
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		Arc::make_mut(self.working.live_catalog_mut()).acknowledge(id, token.version(), stamp)?;
		self.insert_write(ConflictKey::LiveDefinition(id), 0);
		self.write_bytes += SCHEMA_WRITE_BYTES;
		self.mark_range_change(ConflictKey::LiveCatalogRange);
		self.mutations.push(Mutation::AcknowledgeLive(token.clone()));
		Ok(())
	}

	/// Atomically closes an incarnation and stages a new query/capture
	/// identity.
	///
	/// # Errors
	///
	/// Returns registration/close errors; failure restores both private
	/// changes.
	pub fn reregister_live(
		&mut self,
		old: LiveId,
		query: LiveQuery,
		options: LiveOptions,
	) -> FerrosGrynnResult<LiveId> {
		self.run_gql_statement(|tx| {
			tx.prepare_schema_mutation()?;
			let savepoint = tx.gql_savepoint();
			let result = (|| {
				tx.close_live(old)?;
				tx.register_live(query, options)
			})();
			if result.is_err() {
				tx.restore_gql_savepoint(savepoint);
			}
			result
		})
	}

	/// Reads owned metadata from the transaction's exact private snapshot.
	///
	/// # Errors
	///
	/// Returns snapshot expiry or missing-definition errors.
	pub fn live_info(&mut self, id: LiveId) -> FerrosGrynnResult<LiveInfo> {
		self.read_live_info(id, None)
	}

	/// GQL metadata uses the statement root, without Read Committed refresh.
	pub(crate) fn query_live_info(
		&self,
		snapshot: &crate::datastore::QuerySnapshot,
		id: LiveId,
		maximum_bytes: usize,
	) -> FerrosGrynnResult<LiveInfo> {
		self.ensure_live()?;
		self.materialize_live_info(snapshot.logical_root(), id, Some(maximum_bytes))
	}

	/// Routing uses the statement-visible catalog, including ReadCommitted
	/// refresh, without constructing metadata or swallowing read failures.
	pub(in crate::datastore) fn contains_live_query(
		&mut self,
		id: LiveId,
	) -> FerrosGrynnResult<bool> {
		self.prepare_read()?;
		Ok(self.working.live_catalog().queries.contains_key(&id))
	}

	/// One snapshot/read-set authority serves SDK and bounded introspection.
	fn read_live_info(
		&mut self,
		id: LiveId,
		maximum_bytes: Option<usize>,
	) -> FerrosGrynnResult<LiveInfo> {
		self.prepare_read()?;
		self.materialize_live_info(&self.working, id, maximum_bytes)
	}

	/// SDK and GQL share field projection and SSI predicates over one root.
	fn materialize_live_info(
		&self,
		root: &crate::datastore::DatabaseRoot,
		id: LiveId,
		maximum_bytes: Option<usize>,
	) -> FerrosGrynnResult<LiveInfo> {
		self.record_read(ConflictKey::LiveDefinition(id));
		self.record_read(ConflictKey::LiveCatalogRange);
		let definition =
			root.live_catalog().queries.get(&id).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("LIVE identity is not registered")
			})?;
		let info = match maximum_bytes {
			Some(maximum) => definition.info_bounded(root, maximum)?,
			None => definition.info(root)?,
		};
		for binding in info.bindings() {
			self.record_read(ConflictKey::LiveBinding(binding.id()));
		}
		Ok(info)
	}

	/// Stable source identities must resolve in the registering snapshot.
	fn validate_live_source(&self, source: &LiveSource) -> FerrosGrynnResult<()> {
		validate_source(&self.working, source)?;
		let class = match source {
			LiveSource::Class(class)
			| LiveSource::GraphClass {
				class,
				..
			}
			| LiveSource::GraphClassExpr {
				class,
				..
			} => self.working.schema.class_by_id(*class),
			LiveSource::Record(record)
			| LiveSource::GraphRecord {
				record,
				..
			}
			| LiveSource::GraphRecordExpr {
				record,
				..
			} => self.working.schema.class(record.class()),
			LiveSource::Graph(_) | LiveSource::GraphExpr(_) => None,
		};
		if !matches!(source, LiveSource::Graph(_) | LiveSource::GraphExpr(_)) && class.is_none() {
			return Err(FerrosGrynnError::invalid_argument("LIVE source class does not exist"));
		}
		if let LiveSource::Graph(graph)
		| LiveSource::GraphClass {
			graph,
			..
		}
		| LiveSource::GraphRecord {
			graph,
			..
		} = source
		{
			self.record_read(ConflictKey::GraphDefinition(graph.id()));
		}
		if let Some(class) = class.and_then(|class| class.id()) {
			self.record_read(ConflictKey::BaseClassRange(class));
		}
		Ok(())
	}
}

/// Revalidates stable source resolution during private registration and replay.
pub(super) fn validate_source(
	root: &crate::datastore::DatabaseRoot,
	source: &LiveSource,
) -> FerrosGrynnResult<()> {
	let present = match source {
		LiveSource::Class(class)
		| LiveSource::GraphClass {
			class,
			..
		}
		| LiveSource::GraphClassExpr {
			class,
			..
		} => root.schema.class_by_id(*class).is_some(),
		LiveSource::Record(record)
		| LiveSource::GraphRecord {
			record,
			..
		}
		| LiveSource::GraphRecordExpr {
			record,
			..
		} => root.schema.class(record.class()).is_some(),
		LiveSource::Graph(_) | LiveSource::GraphExpr(_) => true,
	};
	if !present {
		return Err(FerrosGrynnError::invalid_argument("LIVE source class does not exist"));
	}
	if let LiveSource::Graph(graph)
	| LiveSource::GraphClass {
		graph,
		..
	}
	| LiveSource::GraphRecord {
		graph,
		..
	} = source
		&& root.schema.graph(graph.id()).is_none()
	{
		return Err(FerrosGrynnError::invalid_argument(
			"LIVE source graph identity does not resolve",
		));
	}
	Ok(())
}

/// Captures the original owner independently of its reusable textual name.
pub(super) fn source_class(
	root: &crate::datastore::DatabaseRoot,
	source: &LiveSource,
) -> FerrosGrynnResult<Option<crate::datastore::ClassId>> {
	match source {
		LiveSource::Class(class)
		| LiveSource::GraphClass {
			class,
			..
		}
		| LiveSource::GraphClassExpr {
			class,
			..
		} => Ok(Some(*class)),
		LiveSource::Record(record)
		| LiveSource::GraphRecord {
			record,
			..
		}
		| LiveSource::GraphRecordExpr {
			record,
			..
		} => root
			.schema
			.class(record.class())
			.and_then(|class| class.id())
			.map(Some)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("LIVE record owner is missing")),
		LiveSource::Graph(_) | LiveSource::GraphExpr(_) => Ok(None),
	}
}

#[cfg(test)]
/// Regression tests for live behavior.
mod tests {
	use super::*;
	use crate::{
		datastore::{ClassSchema, Datastore, InMemoryOptions},
		graph::AtomKind,
	};

	/// Publication and old snapshots preserve independent incarnation states.
	#[test]
	fn live_info_history_reads_prevent_stale_serializable_writes() {
		use crate::{
			changefeed::{ChangefeedConfig, ChangefeedSource},
			datastore::{IsolationLevel, TransactionAccess, TransactionOptions},
			test_assertions::AssertResult as _,
			values::PropertyMap,
		};
		let store = Datastore::in_memory();
		let mut setup = store.begin();
		let class = setup
			.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok())
			.assert_ok();
		setup
			.configure_changefeed(
				ChangefeedSource::Class(class),
				ChangefeedConfig::new("P1D".parse().assert_ok()).assert_ok(),
			)
			.assert_ok();
		let id = setup
			.register_live(LiveQuery::new(LiveSource::Class(class)), LiveOptions::default())
			.assert_ok();
		setup.commit().assert_ok();
		let mut reader = store.begin_with(TransactionOptions::new(
			IsolationLevel::Serializable,
			TransactionAccess::ReadWrite,
		));
		assert_eq!(
			reader.live_info(id).assert_ok().history().map(|history| history.last_stamp().get()),
			Some(0)
		);
		let mut writer = store.begin();
		writer.create_atom("item:new".parse().assert_ok(), PropertyMap::new()).assert_ok();
		let changed = writer.commit().assert_ok().versionstamp();
		assert_eq!(
			store.begin().live_info(id).assert_ok().history().map(|history| history.last_stamp()),
			Some(changed)
		);
		reader.create_atom("item:stale".parse().assert_ok(), PropertyMap::new()).assert_ok();
		assert!(reader.commit().is_err(), "owned metadata must participate in SSI");
	}

	/// Owned metadata caps cover frozen vectors and names before cloning them.
	#[test]
	fn live_info_vector_admission_and_fixed_statement_snapshot() {
		use crate::{
			datastore::{
				FunctionId,
				IsolationLevel,
				SchemaVersion,
				TransactionAccess,
				TransactionOptions,
			},
			test_assertions::AssertResult as _,
		};
		let store = Datastore::in_memory();
		let mut setup = store.begin();
		let class = setup
			.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok())
			.assert_ok();
		let id = setup
			.register_live(LiveQuery::new(LiveSource::Class(class)), LiveOptions::default())
			.assert_ok();
		setup.commit().assert_ok();
		let mut reader = store.begin_with(TransactionOptions::new(
			IsolationLevel::ReadCommitted,
			TransactionAccess::ReadOnly,
		));
		let snapshot = reader.query_snapshot().assert_ok();
		let mut close = store.begin();
		close.close_live(id).assert_ok();
		close.commit().assert_ok();
		assert_eq!(reader.live_info(id).assert_ok().state(), LiveState::Closed);
		let fixed = reader.query_live_info(&snapshot, id, 1 << 20).assert_ok();
		assert_eq!(fixed.state(), LiveState::Active);
		assert_eq!(fixed.snapshot().get(), snapshot.commit_ts());
		let mut definition = snapshot.logical_root().live_catalog().queries[&id].as_ref().clone();
		let function = FunctionId::new(1)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture function id"))
			.assert_ok();
		definition.functions = vec![(function, SchemaVersion::INITIAL); 8192];
		assert!(definition.info_bounded(snapshot.logical_root(), 64 * 1024).is_err());
		definition.functions.clear();
		definition.record_sources = vec![(crate::values::Text::from("x".repeat(64 * 1024)), class)];
		assert!(definition.info_bounded(snapshot.logical_root(), 64 * 1024).is_err());
		definition.record_sources.clear();
		let mut root = snapshot.logical_root().as_ref().clone();
		root.commit_ts = definition.starting_after + 8192;
		let source =
			crate::changefeed::ChangefeedId::from_uuid(crate::values::Uuid::now_v7()).assert_ok();
		let policy = crate::datastore::LiveCapturePolicy {
			source,
			revision: SchemaVersion::INITIAL,
			retention_ns: 86_400_000_000_000,
		};
		let epochs = (0..4097_u64)
			.map(|at| crate::datastore::LiveCaptureEpoch {
				started_at: definition.starting_after + at + 1,
				policy: (at % 2 == 0).then_some(policy),
			})
			.collect::<Vec<_>>();
		Arc::make_mut(root.live_catalog_mut()).histories.insert(
			id,
			Arc::new(crate::datastore::LiveHistoryMetadata {
				version: definition.version,
				last_stamp: 0,
				purged_through: 0,
				purged_time_ns: 0,
				retained_page: 0,
				retained_groups: 0,
				epochs,
			}),
		);
		let slots = 4097 * std::mem::size_of::<crate::live::LiveCaptureEpoch>();
		let bytes = std::mem::size_of::<LiveInfo>() + slots + 4096;
		let info = definition.info_bounded(&root, bytes).assert_ok();
		let history = info
			.history()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture public epochs"))
			.assert_ok();
		assert_eq!(
			history.epochs.capacity(),
			4097,
			"fallible collection must not grow beyond admitted slots"
		);
		assert!(definition.info_bounded(&root, slots - 1).is_err());
	}

	/// Publication and old snapshots preserve independent incarnation states.
	#[test]
	fn live_registration_replacement_and_close_are_transactional() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let mut tx = store.begin();
		let class =
			tx.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
		let query = LiveQuery::new(LiveSource::Class(class));
		let id = tx.register_live(query.clone(), LiveOptions::default()).assert_ok();
		assert_eq!(tx.live_info(id).assert_ok().state(), LiveState::Pending);
		assert!(store.begin().live_info(id).is_err());
		let stamp = tx.commit().assert_ok().versionstamp();
		let mut old = store.begin();
		let info = old.live_info(id).assert_ok();
		assert_eq!(info.state(), LiveState::Active);
		assert_eq!(info.defined_at(), stamp);
		assert_eq!(info.cursor().versionstamp(), stamp);
		let mut first = store.begin();
		let mut second = store.begin();
		let replacement = first.reregister_live(id, query, LiveOptions::default()).assert_ok();
		second.close_live(id).assert_ok();
		assert_ne!(replacement, id);
		first.commit().assert_ok();
		assert!(second.commit().is_err());
		assert_eq!(store.begin().live_info(id).assert_ok().state(), LiveState::Closed);
		assert_eq!(store.begin().live_info(replacement).assert_ok().state(), LiveState::Active);
		assert_eq!(old.live_info(id).assert_ok().state(), LiveState::Active);
	}

	/// Savepoints discard recipes, and failed atomic replacement keeps the old
	/// one.
	#[test]
	fn live_savepoint_and_replacement_failure_preserve_registry() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let mut tx = store.begin();
		let class =
			tx.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
		let query = LiveQuery::new(LiveSource::Class(class));
		let saved = tx.gql_savepoint();
		let discarded = tx.register_live(query.clone(), LiveOptions::default()).assert_ok();
		tx.restore_gql_savepoint(saved);
		assert!(tx.live_info(discarded).is_err());
		let kept = tx.register_live(query, LiveOptions::default()).assert_ok();
		tx.commit().assert_ok();
		let mut replace = store.begin();
		// Replacement is atomic even when ordinary mutation rollback is
		// disabled.
		replace.rollback_mutation_errors = false;
		assert!(
			replace
				.reregister_live(
					kept,
					LiveQuery::new(LiveSource::Record("missing:a".parse().assert_ok())),
					LiveOptions::default(),
				)
				.is_err()
		);
		assert_eq!(replace.live_info(kept).assert_ok().state(), LiveState::Active);
		let before = replace.mutations.len();
		replace.close_live(kept).assert_ok();
		replace.close_live(kept).assert_ok();
		assert_eq!(replace.mutations.len(), before + 1);
		replace.commit().assert_ok();
	}

	/// Resource rejection does not retain a definition or commit an empty
	/// write.
	#[test]
	fn live_registration_budget_and_aborted_transaction_do_not_publish() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let mut setup = store.begin();
		let class = setup
			.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok())
			.assert_ok();
		let stamp = setup.commit().assert_ok().versionstamp();
		let mut discarded = store.begin();
		let id = discarded
			.register_live(LiveQuery::new(LiveSource::Class(class)), LiveOptions::default())
			.assert_ok();
		drop(discarded);
		assert!(store.begin().live_info(id).is_err());
		assert_eq!(store.begin().commit().assert_ok().versionstamp(), stamp);
		let limits_defaults = InMemoryOptions::default();
		let limits = InMemoryOptions {
			max_write_set_bytes: SCHEMA_WRITE_BYTES,
			..limits_defaults
		};
		let limited = Datastore::from_root_for_test(limits, (*store.inner.current_root()).clone());
		let mut rejected = limited.begin();
		assert!(
			rejected
				.register_live(LiveQuery::new(LiveSource::Class(class)), LiveOptions::default())
				.is_err()
		);
		assert!(rejected.working.live_catalog().queries.is_empty());
		assert!(rejected.mutations.is_empty());
	}

	/// Empty sources retain effect/capture checks, and lowering checks budgets.
	#[test]
	fn live_empty_graph_inputs_and_lowering_admission_are_checked() {
		use crate::{
			graph::GraphKind,
			live::{LiveExpr, LivePath, LiveProjection},
			test_assertions::AssertResult as _,
			values::Value,
		};
		let store = Datastore::in_memory();
		let mut tx = store.begin();
		let graph = tx.define_graph("scope", GraphKind::Graph).assert_ok();
		for expression in [
			LiveExpr::Call {
				name: "time::now".into(),
				arguments: Vec::new(),
			},
			LiveExpr::Parameter("missing".into()),
		] {
			let query = LiveQuery::new(LiveSource::Graph(graph.clone()))
				.with_projection(LiveProjection::Value(expression));
			assert!(tx.register_live(query, LiveOptions::default()).is_err());
		}
		assert!(tx.working.live_catalog().queries.is_empty());
		let query = LiveQuery::new(LiveSource::Graph(graph.clone()))
			.with_projection(LiveProjection::Value(LiveExpr::Literal(Value::Null)));
		tx.register_live(query, LiveOptions::default()).assert_ok();
		for index in 0..10 {
			tx.define_class(ClassSchema::new(format!("c{index}"), AtomKind::Node, []).assert_ok())
				.assert_ok();
		}
		for fetch in [false, true] {
			let path = LivePath::field("x".repeat(16 * 1024)).assert_ok();
			let query = if fetch {
				LiveQuery::new(LiveSource::Graph(graph.clone())).with_fetch(vec![path])
			} else {
				LiveQuery::new(LiveSource::Graph(graph.clone()))
					.with_projection(LiveProjection::Value(LiveExpr::Field(path)))
			};
			let options = LiveOptions::default().with_budgets(256 * 1024, 1_000_000).assert_ok();
			let error = tx
				.register_live(query, options)
				.err()
				.ok_or_else(|| {
					FerrosGrynnError::invalid_argument("oversized repeated names were admitted")
				})
				.assert_ok();
			assert!(error.to_string().contains("LIVE registration scratch"), "{error}");
		}
		let query = LiveQuery::new(LiveSource::Graph(graph))
			.with_projection(LiveProjection::Value(LiveExpr::Literal(Value::Null)));
		assert!(
			tx.register_live(query, LiveOptions::default().with_budgets(1 << 20, 1).assert_ok())
				.is_err()
		);
		assert_eq!(tx.working.live_catalog().queries.len(), 1);
	}

	/// Wide schemas and named bodies cannot bypass registration admission.
	#[test]
	fn live_registration_admits_schema_and_pure_compilation_before_analysis() {
		use crate::{
			datastore::{ClassField, FunctionEffect, SchemaFunction},
			live::{LiveExpr, LiveProjection},
			test_assertions::AssertResult as _,
			values::Type,
		};
		let store = Datastore::in_memory();
		let mut tx = store.begin();
		let wide = tx
			.define_class(
				ClassSchema::new(
					"wide",
					AtomKind::Node,
					(0..1000).map(|index| {
						ClassField::new(format!("f{index}"), Type::Array(Box::new(Type::Int)))
					}),
				)
				.assert_ok(),
			)
			.assert_ok();
		for (scratch, work) in [(64 * 1024, 1_000_000), (1 << 20, 1)] {
			assert!(
				tx.register_live(
					LiveQuery::new(LiveSource::Class(wide)),
					LiveOptions::default().with_budgets(scratch, work).assert_ok()
				)
				.is_err()
			);
		}
		let rich = tx
			.define_class(
				ClassSchema::new("rich", AtomKind::Node, [ClassField::new(
					"payload",
					Type::Union(vec![Type::Int; 4096]),
				)])
				.assert_ok(),
			)
			.assert_ok();
		for segments in [
			vec!["payload".into()],
			vec!["PAYLOAD".into()],
			vec!["rich".into(), "payload".into()],
			vec!["RICH".into(), "PAYLOAD".into()],
		] {
			let path = crate::live::LivePath::new(segments).assert_ok();
			let fields = (0..100)
				.map(|index| {
					Ok(crate::live::LiveField::new(LiveExpr::Field(path.clone()))
						.with_alias(crate::live::LivePath::field(format!("p{index}"))?))
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()
				.assert_ok();
			let query = LiveQuery::new(LiveSource::Class(rich))
				.with_projection(LiveProjection::Fields(fields));
			let error = tx
				.register_live(
					query.clone(),
					LiveOptions::default().with_budgets(4 << 20, 1_000_000).assert_ok(),
				)
				.err()
				.ok_or_else(|| {
					FerrosGrynnError::invalid_argument("rich type multiplicity was admitted")
				})
				.assert_ok();
			assert!(error.to_string().contains("LIVE registration scratch"), "{error}");
			let saved = tx.gql_savepoint();
			// The optimizer copy is now admitted too; a narrower projection
			// demonstrates the same rich type contract within the default cap.
			let LiveProjection::Fields(fields) = query.projection() else {
				return {
					let test_result: FerrosGrynnResult<()> =
						Err(FerrosGrynnError::invalid_argument("fixture projection"));
					test_result.assert_ok();
				};
			};
			let query = query
				.clone()
				.with_projection(LiveProjection::Fields(fields.iter().take(10).cloned().collect()));
			tx.register_live(query, LiveOptions::default()).assert_ok();
			tx.restore_gql_savepoint(saved);
		}
		let narrow =
			tx.define_class(ClassSchema::new("narrow", AtomKind::Node, []).assert_ok()).assert_ok();
		tx.define_function(
			SchemaFunction::new(
				"fn",
				"large",
				Vec::new(),
				Type::String,
				FunctionEffect::Pure,
				false,
				None,
				format!("{{ RETURN \"{}\"; }}", "x".repeat(8192)),
			)
			.assert_ok(),
		)
		.assert_ok();
		let query = LiveQuery::new(LiveSource::Class(narrow)).with_projection(
			LiveProjection::Value(LiveExpr::Call {
				name: "fn::large".into(),
				arguments: Vec::new(),
			}),
		);
		let error = tx
			.register_live(
				query.clone(),
				LiveOptions::default().with_budgets(64 * 1024, 1_000_000).assert_ok(),
			)
			.err()
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("oversized PURE compiler input was admitted")
			})
			.assert_ok();
		assert!(error.to_string().contains("PURE compiler scratch"), "{error}");
		let error = tx
			.register_live(
				query,
				LiveOptions::default().with_budgets(64 * 1024 * 1024, 10).assert_ok(),
			)
			.err()
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("oversized PURE compiler work was admitted")
			})
			.assert_ok();
		assert!(error.to_string().contains("PURE compiler work"), "{error}");
		assert!(tx.working.live_catalog().queries.is_empty());
	}

	/// Record names never rebind within a captured ReadCommitted statement.
	#[test]
	fn live_record_registration_keeps_statement_owner_until_validation() {
		use crate::{
			datastore::{IsolationLevel, TransactionAccess, TransactionOptions},
			test_assertions::AssertResult as _,
		};
		for scoped in [false, true] {
			let store = Datastore::in_memory();
			let mut setup = store.begin();
			let owner = setup
				.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok())
				.assert_ok();
			let graph = setup.define_graph("single", crate::graph::GraphKind::Graph).assert_ok();
			setup.commit().assert_ok();
			let mut reader = store.begin_with(TransactionOptions::new(
				IsolationLevel::ReadCommitted,
				TransactionAccess::ReadWrite,
			));
			let id = reader
				.run_gql_statement(|reader| {
					let mut writer = store.begin();
					writer.remove_class("person")?;
					let replacement =
						writer.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
					assert_ne!(owner, replacement);
					writer.commit()?;
					reader.register_live(
						LiveQuery::new(if scoped {
							LiveSource::GraphRecord {
								graph: graph.clone(),
								record: "person:a".parse()?,
							}
						} else {
							LiveSource::Record("person:a".parse()?)
						}),
						LiveOptions::default(),
					)
				})
				.assert_ok();
			assert_eq!(reader.working.live_catalog().queries[&id].source_class, Some(owner));
			assert!(reader.commit().is_err());
			assert!(store.begin().live_info(id).is_err());
		}
	}

	/// Nested sources retain their class incarnation even before any matching
	/// occurrence exists; a reused name pauses the binding instead of
	/// rebinding.
	#[test]
	fn live_graph_record_nested_binding_keeps_owner() {
		use crate::{
			live::{LiveBindingOptions, LiveExpr, LiveProjection},
			test_assertions::AssertResult as _,
		};
		let store = Datastore::in_memory();
		let mut setup = store.begin();
		let owner = setup
			.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok())
			.assert_ok();
		let graph = setup.define_graph("single", crate::graph::GraphKind::Graph).assert_ok();
		let id = setup
			.define_live_binding(
				LiveExpr::Select(Box::new(
					LiveQuery::new(LiveSource::GraphRecord {
						graph,
						record: "person:a".parse().assert_ok(),
					})
					.with_projection(LiveProjection::Value(LiveExpr::Call {
						name: "count".into(),
						arguments: Vec::new(),
					})),
				)),
				crate::values::Object::new(),
				LiveBindingOptions::default(),
			)
			.assert_ok();
		setup.commit().assert_ok();
		assert_eq!(store.begin().live_binding_value(id).assert_ok(), crate::values::Value::from(0));
		let mut update = store.begin();
		update.remove_class("person").assert_ok();
		let replacement = update
			.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok())
			.assert_ok();
		assert_ne!(owner, replacement);
		update.commit().assert_ok();
		assert_eq!(
			store.begin().live_binding_info(id).assert_ok().state(),
			crate::live::LiveState::Paused
		);
		assert!(store.begin().live_binding_value(id).is_err());
	}

	/// Root-bound values preserve existing SELECT semantics and exact
	/// identities.
	#[test]
	fn live_root_expression_select_fetch_and_frozen_inputs() {
		use std::{cell::RefCell, collections::BTreeSet};

		use crate::{
			datastore::ClassField,
			live::{
				LiveBinaryOperator,
				LiveBindingId,
				LiveBindingOptions,
				LiveExpr,
				LivePath,
				LiveProjection,
			},
			test_assertions::AssertResult as _,
			values::{Object, PropertyMap, Type, Value},
		};
		let store = Datastore::in_memory();
		let mut tx = store.begin();
		let class = tx
			.define_class(
				ClassSchema::new("person", AtomKind::Node, [
					ClassField::new("age", Type::Int),
					ClassField::new("manager", Type::Option(Box::new(Type::Record))),
				])
				.assert_ok(),
			)
			.assert_ok();
		let age = tx.intern_property("age").assert_ok();
		let manager = tx.intern_property("manager").assert_ok();
		let alice = "person:alice".parse().assert_ok();
		let bob: crate::values::RecordId = "person:bob".parse().assert_ok();
		tx.create_atom(
			alice,
			PropertyMap::from_iter([
				(age, Value::from(9_007_199_254_740_993_i64)),
				(manager, Value::Record(Box::new(bob.clone()))),
			]),
		)
		.assert_ok();
		tx.create_atom(bob.clone(), PropertyMap::from_iter([(age, Value::from(42_i64))]))
			.assert_ok();
		let graph = tx.define_graph("scope", crate::graph::GraphKind::Graph).assert_ok();
		tx.attach_node(graph.id(), &"person:alice".parse().assert_ok(), None, None).assert_ok();
		let root = Arc::new(tx.working.clone());
		let dependencies = RefCell::new(BTreeSet::new());
		let observe = |key| {
			dependencies.borrow_mut().insert(key);
		};
		let mut parameters = Object::new();
		parameters.insert("fixed", Value::from(3_i64));
		let binding = LiveBindingId::from_uuid(crate::values::Uuid::now_v7()).assert_ok();
		let binding_value = Value::from(5_i64);
		let values = |id| {
			if id == binding {
				Ok(&binding_value)
			} else {
				Err(FerrosGrynnError::invalid_argument("missing test binding"))
			}
		};
		let evaluate = |expression: &LiveExpr| {
			crate::gql::live_lowering::evaluate_binding(
				&root,
				expression,
				&parameters,
				&values,
				&LiveBindingOptions::default(),
				tx.query_scratch_root(),
				&observe,
			)
			.map(|evaluation| evaluation.value)
		};
		assert_eq!(
			evaluate(
				&LiveExpr::Parameter("fixed".into())
					.binary(LiveBinaryOperator::Add, LiveExpr::Binding(binding))
			)
			.assert_ok(),
			Value::from(8_i64)
		);
		let count = |source| {
			LiveExpr::Select(Box::new(LiveQuery::new(source).with_projection(
				LiveProjection::Value(LiveExpr::Call {
					name: "count".into(),
					arguments: Vec::new(),
				}),
			)))
		};
		assert_eq!(evaluate(&count(LiveSource::Class(class))).assert_ok(), Value::from(2_i64));
		assert_eq!(
			evaluate(&count(LiveSource::GraphClass {
				graph: graph.clone(),
				class
			}))
			.assert_ok(),
			Value::from(1_i64)
		);
		let query = LiveExpr::Select(Box::new(
			LiveQuery::new(LiveSource::Record("person:alice".parse().assert_ok()))
				.with_fetch(vec![LivePath::field("manager").assert_ok()]),
		));
		let Value::Object(record) = evaluate(&query).assert_ok() else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("root SELECT returned no record"));
				test_result.assert_ok();
			};
		};
		assert_eq!(record.get("age"), Some(&Value::from(9_007_199_254_740_993_i64)));
		let Value::Object(fetched) = record
			.get("manager")
			.ok_or_else(|| FerrosGrynnError::invalid_argument("missing manager"))
			.assert_ok()
		else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("FETCH did not resolve its record"));
				test_result.assert_ok();
			};
		};
		assert_eq!(fetched.get("age"), Some(&Value::from(42_i64)));
		assert!(dependencies.borrow().contains(&ConflictKey::BaseClassRange(class)));
		assert!(dependencies.borrow().contains(&ConflictKey::GraphClassRange(graph.id(), class)));
		let bob_id = root
			.resolve_atom(&bob)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("missing Bob"))
			.assert_ok();
		assert!(dependencies.borrow().contains(&ConflictKey::BaseAtom(bob_id)));
		let limited = LiveBindingOptions::default().with_budgets(1024, 1_000_000).assert_ok();
		assert!(
			crate::gql::live_lowering::evaluate_binding(
				&root,
				&query,
				&parameters,
				&values,
				&limited,
				tx.query_scratch_root(),
				&observe
			)
			.is_err()
		);
	}

	/// Shared SELECT special materialization keeps named calls and lazy
	/// branches.
	#[test]
	fn live_root_expression_named_select_and_short_circuit() {
		use crate::{
			datastore::{FunctionEffect, SchemaFunction, SchemaFunctionParameter},
			live::{LiveBinaryOperator, LiveBindingOptions, LiveExpr, LiveProjection},
			test_assertions::AssertResult as _,
			values::{Object, Type, Value},
		};
		let store = Datastore::in_memory();
		let mut tx = store.begin();
		let class =
			tx.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
		tx.define_function(
			SchemaFunction::new(
				"fn",
				"identity",
				vec![SchemaFunctionParameter::new("value", Type::Any).assert_ok()],
				Type::Any,
				FunctionEffect::Pure,
				false,
				None,
				"{ RETURN $value; }",
			)
			.assert_ok(),
		)
		.assert_ok();
		let root = Arc::new(tx.working.clone());
		let captures = Object::new();
		let missing = |_id| Err(FerrosGrynnError::invalid_argument("unexpected binding"));
		let observe = |_key| {};
		let evaluate = |expression: &LiveExpr| {
			crate::gql::live_lowering::evaluate_binding(
				&root,
				expression,
				&captures,
				&missing,
				&LiveBindingOptions::default(),
				tx.query_scratch_root(),
				&observe,
			)
			.map(|evaluation| evaluation.value)
		};
		let count =
			LiveExpr::Select(Box::new(LiveQuery::new(LiveSource::Class(class)).with_projection(
				LiveProjection::Value(LiveExpr::Call {
					name: "count".into(),
					arguments: Vec::new(),
				}),
			)));
		let call = LiveExpr::Call {
			name: "fn::identity".into(),
			arguments: vec![count],
		};
		assert_eq!(evaluate(&call).assert_ok(), Value::from(0_i64));
		tx.create_atom("person:a".parse().assert_ok(), crate::values::PropertyMap::new())
			.assert_ok();
		let root = Arc::new(tx.working.clone());
		let failing = LiveExpr::Select(Box::new(
			LiveQuery::new(LiveSource::Class(class)).with_projection(LiveProjection::Value(
				LiveExpr::Literal(Value::from(1_i64))
					.binary(LiveBinaryOperator::Divide, LiveExpr::Literal(Value::from(0_i64))),
			)),
		));
		let expression = LiveExpr::Literal(Value::Bool(false)).binary(
			LiveBinaryOperator::And,
			failing.binary(LiveBinaryOperator::Equal, LiveExpr::Literal(Value::from(0_i64))),
		);
		assert_eq!(
			crate::gql::live_lowering::evaluate_binding(
				&root,
				&expression,
				&captures,
				&missing,
				&LiveBindingOptions::default(),
				tx.query_scratch_root(),
				&observe
			)
			.assert_ok()
			.value,
			Value::Bool(false)
		);
		let aggregate = LiveExpr::Call {
			name: "count".into(),
			arguments: Vec::new(),
		};
		assert!(
			crate::gql::live_lowering::evaluate_binding(
				&root,
				&aggregate,
				&captures,
				&missing,
				&LiveBindingOptions::default(),
				tx.query_scratch_root(),
				&observe
			)
			.is_err()
		);
	}

	/// Recovered stamps are bounded by the root clock, independent of
	/// filenames.
	#[test]
	fn live_catalog_recovery_requires_authoritative_commit_bounds() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let mut tx = store.begin();
		let class =
			tx.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
		let id = tx
			.register_live(LiveQuery::new(LiveSource::Class(class)), LiveOptions::default())
			.assert_ok();
		let stamp = tx.commit().assert_ok().versionstamp().get();
		let root = store.begin().working;
		root.live_catalog().validate_recovered(stamp).assert_ok();
		for ack in [false, true] {
			let mut catalog = root.live_catalog().as_ref().clone();
			let entry = Arc::make_mut(
				catalog
					.queries
					.get_mut(&id)
					.ok_or_else(|| FerrosGrynnError::invalid_argument("missing test definition"))
					.assert_ok(),
			);
			if ack {
				entry.acknowledged = stamp + 1;
			} else {
				entry.published_at = stamp + 1;
			}
			assert!(catalog.validate_recovered(stamp).is_err());
		}
	}
}

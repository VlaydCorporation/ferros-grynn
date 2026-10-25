//! Transactional capture definitions and retained logical metadata.

use std::sync::Arc;

use super::{Mutation, Transaction};
use crate::{
	changefeed::{
		ChangefeedConfig,
		ChangefeedId,
		ChangefeedInfo,
		ChangefeedSelector,
		ChangefeedSource,
		ChangefeedState,
	},
	datastore::{ConflictKey, consts::SCHEMA_WRITE_BYTES},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::Text,
};

impl Transaction {
	/// Registers retained-source conflicts when schema removal stops capture.
	pub(super) fn retire_missing_changefeeds(&mut self) -> FerrosGrynnResult<()> {
		for source in self.working.retire_missing_changefeeds(self.snapshot.commit_ts)? {
			self.insert_write(ConflictKey::ChangefeedDefinition(source), 0);
			self.mark_range_change(ConflictKey::ChangefeedCatalogRange);
		}
		Ok(())
	}

	/// Configures capture for a class or graph in this transaction.
	///
	/// # Errors
	///
	/// Returns a missing-owner, read-only, import-DDL, revision, or resource
	/// error.
	///
	/// ```
	/// # use fg_engine as ferros_grynn;
	/// # fn main() -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
	/// use ferros_grynn::{
	/// 	changefeed::{ChangefeedConfig, ChangefeedSource},
	/// 	datastore::{ClassSchema, Datastore},
	/// 	graph::AtomKind,
	/// 	values::Duration,
	/// };
	/// let store = Datastore::in_memory();
	/// let mut tx = store.begin();
	/// let class = tx.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	/// let retention = "P7D".parse::<Duration>().map_err(|error| {
	/// 	ferros_grynn::gql_status::FerrosGrynnError::invalid_argument(error.to_string())
	/// })?;
	/// let id =
	/// 	tx.configure_changefeed(ChangefeedSource::Class(class), ChangefeedConfig::new(retention)?)?;
	/// tx.commit()?;
	/// assert_eq!(store.begin().changefeed_info(id)?.id(), id);
	/// # Ok(())
	/// # }
	/// ```
	#[allow(
		clippy::tabs_in_doc_comments,
		reason = "nightly rustfmt uses the required hard tabs in Rustdoc examples"
	)]
	pub fn configure_changefeed(
		&mut self,
		source: ChangefeedSource,
		config: ChangefeedConfig,
	) -> FerrosGrynnResult<ChangefeedId> {
		self.prepare_schema_mutation()?;
		let name = match source {
			ChangefeedSource::Class(class) => {
				self.working.schema.class_by_id(class).map(|entry| entry.name().clone())
			}
			ChangefeedSource::Graph(graph) => {
				self.working.schema.graph(graph).map(|entry| entry.name().clone())
			}
		}
		.ok_or_else(|| FerrosGrynnError::invalid_argument("changefeed owner does not exist"))?;
		let previous = self.working.changefeeds().for_source(source);
		if let Some(previous) = previous
			&& previous.state == ChangefeedState::Active
			&& previous.config == config
		{
			return Ok(previous.id);
		}
		let id = previous.map_or_else(ChangefeedId::allocate, |entry| entry.id);
		self.store.inner.maintenance.start(&self.store.inner)?;
		self.changefeed_mutation(source, id, name, Some(config))?;
		Ok(id)
	}

	/// Stops capture while preserving retained history until its TTL.
	///
	/// # Errors
	///
	/// Returns a missing-source, read-only, import-DDL, revision, or resource
	/// error.
	pub fn drop_changefeed(&mut self, source: ChangefeedSource) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		let previous =
			self.working.changefeeds().for_source(source).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("changefeed is not configured")
			})?;
		if previous.state != ChangefeedState::Active {
			return Ok(());
		}
		self.changefeed_mutation(source, previous.id, previous.name.clone(), None)
	}

	/// Returns owned metadata from the selected MVCC catalog.
	///
	/// # Errors
	///
	/// Returns a snapshot-age, missing-source, or ambiguous historical-name
	/// error.
	pub fn changefeed_info<Selector: Into<ChangefeedSelector>>(
		&mut self,
		selector: Selector,
	) -> FerrosGrynnResult<ChangefeedInfo> {
		self.changefeed_info_admitted(selector, |_| Ok(()))
	}

	/// Allows query consumers to reserve owned metadata before catalog cloning.
	///
	/// # Errors
	///
	/// Returns ordinary metadata lookup errors or the consumer's admission
	/// error.
	pub(crate) fn changefeed_info_admitted<Selector: Into<ChangefeedSelector>>(
		&mut self,
		selector: Selector,
		admit: impl FnOnce(usize) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<ChangefeedInfo> {
		self.prepare_read()?;
		self.record_read(ConflictKey::ChangefeedCatalogRange);
		let definition = self.resolve_changefeed(&selector.into())?;
		admit(definition.info_memory_bytes()?)?;
		Ok(definition.info(&self.working))
	}

	/// Returns constant-size committed origin/bounds without refreshing the
	/// snapshot.
	///
	/// # Errors
	///
	/// Returns snapshot expiry when the transaction is no longer readable.
	pub(crate) fn query_changefeed_context(
		&self,
	) -> FerrosGrynnResult<(crate::values::Uuid, crate::changefeed::Versionstamp)> {
		self.ensure_live()?;
		Ok((self.snapshot.database_uuid.into(), self.snapshot.commit_ts.into()))
	}

	/// Resolves committed history metadata without refreshing the statement
	/// snapshot.
	///
	/// # Errors
	///
	/// Returns snapshot expiry or missing/ambiguous retained-name errors.
	pub(crate) fn query_changefeed_binding(
		&self,
		selector: &ChangefeedSelector,
	) -> FerrosGrynnResult<Option<crate::changefeed::ChangefeedReadBinding>> {
		self.ensure_live()?;
		self.record_read(ConflictKey::ChangefeedCatalogRange);
		let id = if let ChangefeedSelector::Id(id) = selector {
			*id
		} else {
			self.resolve_changefeed(selector)?.id
		};
		Ok(self
			.snapshot
			.changefeeds()
			.definitions
			.get(&id)
			.map(|definition| definition.read_binding(&self.snapshot)))
	}

	/// Applies private metadata and reserves every configuration mutation.
	fn changefeed_mutation(
		&mut self,
		source: ChangefeedSource,
		id: ChangefeedId,
		name: Text,
		config: Option<ChangefeedConfig>,
	) -> FerrosGrynnResult<()> {
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		Arc::make_mut(self.working.changefeeds_mut()).configure(
			source,
			id,
			name.clone(),
			config,
			self.snapshot.commit_ts,
		)?;
		self.writes
			.entry(ConflictKey::ChangefeedDefinition(source))
			.or_insert_with(|| self.snapshot.version(ConflictKey::ChangefeedDefinition(source)));
		self.write_bytes += SCHEMA_WRITE_BYTES;
		self.mark_range_change(ConflictKey::ChangefeedCatalogRange);
		self.mutations.push(Mutation::ConfigureChangefeed {
			source,
			id,
			name,
			config,
		});
		Ok(())
	}

	/// Resolves exact identities before applying historical name fallback.
	pub(super) fn resolve_changefeed(
		&self,
		selector: &ChangefeedSelector,
	) -> FerrosGrynnResult<&Arc<super::super::ChangefeedDefinition>> {
		let definitions = self.working.changefeeds();
		match selector {
			ChangefeedSelector::Id(id) => definitions.definitions.get(id),
			ChangefeedSelector::Source(source) => definitions.for_source(*source),
			ChangefeedSelector::ClassName(name) => {
				if let Some(class) = self.working.schema.class(name.as_str()) {
					let id = class.id().ok_or_else(|| {
						FerrosGrynnError::invalid_argument("class identity is unpublished")
					})?;
					definitions.for_source(ChangefeedSource::Class(id))
				} else {
					self.historical_changefeed(name, true)?
				}
			}
			ChangefeedSelector::GraphName(name) => {
				if let Some(graph) = self.working.schema.graphs().find(|graph| graph.name() == name)
				{
					definitions.for_source(ChangefeedSource::Graph(graph.id()))
				} else {
					self.historical_changefeed(name, false)?
				}
			}
		}
		.ok_or_else(|| FerrosGrynnError::invalid_argument("changefeed source is not retained"))
	}

	/// Rejects ambiguous retired names instead of choosing a newer incarnation.
	fn historical_changefeed(
		&self,
		name: &Text,
		class: bool,
	) -> FerrosGrynnResult<Option<&Arc<super::super::ChangefeedDefinition>>> {
		let mut matches = self.working.changefeeds().definitions.values().filter(|entry| {
			&entry.name == name && matches!(entry.source, ChangefeedSource::Class(_)) == class
		});
		let found = matches.next();
		if matches.next().is_some() {
			return Err(FerrosGrynnError::invalid_argument(
				"historical changefeed name is ambiguous; use ChangefeedId",
			));
		}
		Ok(found)
	}
}

#[cfg(test)]
/// Regression tests for changefeed behavior.
mod tests {
	use super::*;
	use crate::{
		datastore::{ClassSchema, Datastore, InMemoryOptions, SchemaVersion},
		graph::{AtomKind, GraphKind},
	};

	/// Verifies changefeed configuration is private conflict checked and
	/// retained.
	#[test]
	fn changefeed_configuration_is_private_conflict_checked_and_retained() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let mut setup = store.begin();
		let class = setup
			.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok())
			.assert_ok();
		let graph = setup.define_graph("scope", GraphKind::Graph).assert_ok();
		let source = ChangefeedSource::Class(class);
		let config = ChangefeedConfig::from_nanoseconds(1_000).assert_ok();
		let id = setup.configure_changefeed(source, config).assert_ok();
		let graph_id = setup
			.configure_changefeed(ChangefeedSource::Graph(graph.id()), config.with_full(true))
			.assert_ok();
		assert!(store.begin().changefeed_info(id).is_err());
		let commit = setup.commit().assert_ok();
		assert_eq!(commit.versionstamp().get(), commit.commit_ts);
		let mut old = store.begin();
		let old_info = old.changefeed_info(id).assert_ok();
		assert_eq!(old_info.defined_at(), commit.versionstamp());
		assert_eq!(old_info.capture_epochs().len(), 1);
		let mut first = store.begin();
		let mut second = store.begin();
		assert_eq!(first.configure_changefeed(source, config.with_full(true)).assert_ok(), id);
		second.drop_changefeed(source).assert_ok();
		first.commit().assert_ok();
		let history = store.begin().changefeed_info(id).assert_ok();
		assert_eq!(history.capture_epochs().len(), 2);
		assert_eq!(history.capture_epochs()[0].configuration(), Some(config));
		assert_eq!(history.capture_epochs()[1].configuration(), Some(config.with_full(true)));
		assert!(second.commit().is_err());
		assert_eq!(old.changefeed_info(id).assert_ok(), old_info);
		let mut dropper = store.begin();
		dropper.drop_changefeed(source).assert_ok();
		dropper.commit().assert_ok();
		assert_eq!(store.begin().changefeed_info(id).assert_ok().state(), ChangefeedState::Stopped);
		let mut enable = store.begin();
		assert_eq!(enable.configure_changefeed(source, config).assert_ok(), id);
		enable.commit().assert_ok();
		let mut remove = store.begin();
		remove.remove_class("item").assert_ok();
		remove.remove_graph("scope").assert_ok();
		assert_eq!(remove.changefeed_info(id).assert_ok().state(), ChangefeedState::Retired);
		remove.commit().assert_ok();
		assert_eq!(
			store
				.begin()
				.changefeed_info(ChangefeedSelector::ClassName("item".into()))
				.assert_ok()
				.id(),
			id
		);
		assert_eq!(
			store.begin().changefeed_info(graph_id).assert_ok().state(),
			ChangefeedState::Retired
		);
		let mut recreate = store.begin();
		let replacement = recreate
			.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok())
			.assert_ok();
		assert!(recreate.changefeed_info(ChangefeedSelector::ClassName("item".into())).is_err());
		let next =
			recreate.configure_changefeed(ChangefeedSource::Class(replacement), config).assert_ok();
		assert_ne!(next, id);
		recreate.commit().assert_ok();
		assert_eq!(
			store
				.begin()
				.changefeed_info(ChangefeedSelector::ClassName("item".into()))
				.assert_ok()
				.id(),
			next
		);
		assert_eq!(store.begin().changefeed_info(id).assert_ok().state(), ChangefeedState::Retired);
	}

	/// Verifies changefeed savepoint capacity and clock rollback are explicit.
	#[test]
	fn changefeed_savepoint_capacity_and_clock_rollback_are_explicit() {
		use crate::test_assertions::AssertResult as _;

		let options_defaults = InMemoryOptions::default();
		let options = InMemoryOptions {
			max_write_set_bytes: 8 * SCHEMA_WRITE_BYTES,
			..options_defaults
		};
		let store = Datastore::in_memory_with(options);
		let mut setup = store.begin();
		let class = setup
			.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok())
			.assert_ok();
		setup.commit().assert_ok();
		let mut tx = store.begin();
		let savepoint = tx.gql_savepoint();
		let source = ChangefeedSource::Class(class);
		let config = ChangefeedConfig::from_nanoseconds(1_000).assert_ok();
		let aborted = tx.configure_changefeed(source, config).assert_ok();
		tx.restore_gql_savepoint(savepoint);
		assert!(tx.changefeed_info(aborted).is_err());
		for ordinal in 0..8 {
			tx.configure_changefeed(source, config.with_full(ordinal % 2 == 0)).assert_ok();
		}
		assert!(tx.configure_changefeed(source, config.with_full(true)).is_err());
		let prior = tx.working.committed_at_ns;
		tx.working.advance_commit_time(prior - 1);
		assert_eq!(tx.working.committed_at_ns, prior);
	}

	/// Verifies failed owner retirement restores private schema and journal.
	#[test]
	fn failed_owner_retirement_restores_private_schema_and_journal() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let mut setup = store.begin();
		let class = setup
			.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok())
			.assert_ok();
		let graph = setup.define_graph("scope", GraphKind::Graph).assert_ok();
		let config = ChangefeedConfig::from_nanoseconds(1_000).assert_ok();
		let ids = [
			setup.configure_changefeed(ChangefeedSource::Class(class), config).assert_ok(),
			setup.configure_changefeed(ChangefeedSource::Graph(graph.id()), config).assert_ok(),
		];
		setup.commit().assert_ok();
		let mut tx = store.begin();
		for id in ids {
			let definition = Arc::make_mut(tx.working.changefeeds_mut())
				.definitions
				.get_mut(&id)
				.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture definition is absent"))
				.assert_ok();
			Arc::make_mut(definition).version = SchemaVersion::from_storage(u32::MAX).assert_ok();
		}
		let before = tx.gql_savepoint();
		assert!(tx.remove_class("item").is_err());
		assert!(tx.remove_graph("scope").is_err());
		assert!(tx.working.schema.class_by_id(class).is_some());
		assert!(tx.working.schema.graph(graph.id()).is_some());
		assert_eq!(tx.working.changefeeds(), before.working.changefeeds());
		assert_eq!(tx.mutations.len(), before.mutation_len);
		assert_eq!(tx.write_bytes, before.write_bytes);
	}

	/// Verifies changefeed build graph replacement retires private child
	/// sources.
	#[test]
	fn changefeed_build_graph_replacement_retires_private_child_sources() {
		use crate::{
			test_assertions::AssertResult as _,
			values::{Object, PropertyMap, RecordId},
		};
		let store = Datastore::in_memory();
		let mut setup = store.begin();
		setup.define_class(ClassSchema::new("module", AtomKind::Node, []).assert_ok()).assert_ok();
		setup
			.define_class(ClassSchema::new("document", AtomKind::Node, []).assert_ok())
			.assert_ok();
		let child = setup.define_graph("child", GraphKind::Graph).assert_ok();
		let root = setup.define_graph("root", GraphKind::Metagraph).assert_ok();
		let document = "document:spec".parse::<RecordId>().assert_ok();
		let module = "module:owner".parse::<RecordId>().assert_ok();
		for record in [&document, &module] {
			setup.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		}
		setup.attach_node(child.id(), &document, None, None).assert_ok();
		setup.attach_node(root.id(), &module, None, Some(child.id())).assert_ok();
		setup.commit().assert_ok();
		let mut configure = store.begin();
		for outcome in configure
			.execute(
				"BUILD GRAPH <META> copied FROM (MATCH (document AS d) WITHIN <(module AS owner)> \
				 IN GRAPH root RETURN d, owner) NODES [d, owner]",
				&Object::new(),
			)
			.assert_ok()
		{
			let _result = outcome.result.assert_ok();
		}
		let copied = configure
			.named_graph("copied")
			.assert_ok()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("copied graph is absent"))
			.assert_ok();
		let child_id = configure
			.working
			.schema
			.all_graphs()
			.find(|entry| entry.owner_root() == Some(copied.id()))
			.map(|entry| entry.id())
			.ok_or_else(|| FerrosGrynnError::invalid_argument("owned child graph is absent"))
			.assert_ok();
		let id = configure
			.configure_changefeed(
				ChangefeedSource::Graph(child_id),
				ChangefeedConfig::from_nanoseconds(1_000).assert_ok(),
			)
			.assert_ok();
		configure.commit().assert_ok();
		let mut old = store.begin();
		let mut replace = store.begin();
		for outcome in replace
			.execute("BUILD GRAPH OVERWRITE <META> copied FROM []", &Object::new())
			.assert_ok()
		{
			let _result = outcome.result.assert_ok();
		}
		assert_eq!(replace.changefeed_info(id).assert_ok().state(), ChangefeedState::Retired);
		assert_eq!(old.changefeed_info(id).assert_ok().state(), ChangefeedState::Active);
		drop(replace);
		assert_eq!(store.begin().changefeed_info(id).assert_ok().state(), ChangefeedState::Active);
		let mut replace = store.begin();
		for outcome in replace
			.execute("BUILD GRAPH OVERWRITE <META> copied FROM []", &Object::new())
			.assert_ok()
		{
			let _result = outcome.result.assert_ok();
		}
		replace.commit().assert_ok();
		assert_eq!(store.begin().changefeed_info(id).assert_ok().state(), ChangefeedState::Retired);
		assert_eq!(old.changefeed_info(id).assert_ok().state(), ChangefeedState::Active);
	}
}

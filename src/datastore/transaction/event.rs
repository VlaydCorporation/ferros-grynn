//! Transactional event definitions use ordinary FCW/SSI and schema replay.

use std::sync::Arc;

use super::{Mutation, Transaction};
use crate::{
	datastore::{ConflictKey, EventDefinition, EventId, EventTarget, consts::SCHEMA_WRITE_BYTES},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
};

impl Transaction {
	/// Lists source-owned definitions in stable identity order.
	///
	/// # Errors
	///
	/// Returns transaction-state or metadata admission errors before copying.
	pub fn events_for_source(
		&mut self,
		target: EventTarget,
	) -> FerrosGrynnResult<Vec<Arc<EventDefinition>>> {
		self.prepare_read()?;
		self.record_read(ConflictKey::EventCatalogRange);
		let count =
			self.working.schema.events.values().filter(|event| event.target == target).count();
		let bytes = count
			.checked_mul(std::mem::size_of::<Arc<EventDefinition>>())
			.ok_or_else(|| FerrosGrynnError::operation_overflow("EVENT metadata list"))?;
		self.ensure_capacity(bytes)?;
		let mut definitions = Vec::new();
		definitions.try_reserve_exact(count).map_err(|error| {
			FerrosGrynnError::storage_decode_limit_exceeded(
				"EVENT metadata list",
				bytes as u64,
				self.store.inner.transaction_limits().max_write_set_bytes as u64,
			)
			.with_technical_source(error)
		})?;
		for event in self.working.schema.events.values().filter(|event| event.target == target) {
			if let Some(id) = event.id() {
				self.record_read(ConflictKey::SchemaEvent(id));
			}
			definitions.push(Arc::clone(event));
		}
		Ok(definitions)
	}

	/// Resolves one event definition in the transaction snapshot.
	///
	/// # Errors
	///
	/// Returns an expired or closed transaction error.
	pub fn event(&mut self, id: EventId) -> FerrosGrynnResult<Option<Arc<EventDefinition>>> {
		self.prepare_read()?;
		self.record_read(ConflictKey::SchemaEvent(id));
		Ok(self.working.schema.event(id))
	}

	/// Resolves a source-owned event name, recording an absent-name predicate.
	///
	/// # Errors
	///
	/// Returns an expired or closed transaction error.
	pub fn named_event(
		&mut self,
		target: EventTarget,
		name: &str,
	) -> FerrosGrynnResult<Option<Arc<EventDefinition>>> {
		self.prepare_read()?;
		self.record_read(ConflictKey::EventCatalogRange);
		let event = self.working.schema.named_event(target, name);
		if let Some(id) = event.as_ref().and_then(|event| event.id()) {
			self.record_read(ConflictKey::SchemaEvent(id));
		}
		Ok(event)
	}

	/// Registers a checked source definition without executing user logic.
	///
	/// # Errors
	///
	/// Returns source, syntax, effect, duplicate-name, read-only, capacity or
	/// closed-transaction errors. No definition is installed on failure.
	pub fn define_event(&mut self, event: EventDefinition) -> FerrosGrynnResult<EventId> {
		self.define_event_controlled(event, None)
	}

	/// Shares source compilation limits and interruption with the calling
	/// query.
	pub(crate) fn define_event_controlled(
		&mut self,
		mut event: EventDefinition,
		control: Option<&crate::gql::ExecutionControl>,
	) -> FerrosGrynnResult<EventId> {
		self.prepare_schema_mutation()?;
		if event.id.is_some() {
			return Err(FerrosGrynnError::invalid_argument(
				"define_event requires an unpublished definition",
			));
		}
		self.observe_event_source(event.target)?;
		if self.named_event(event.target, event.name.as_str())?.is_some() {
			return Err(FerrosGrynnError::invalid_argument("EVENT already exists on its source"));
		}
		let bytes = event_definition_bytes(&event)?;
		self.ensure_capacity(bytes)?;
		let scratch = self.event_compiler_scratch(bytes);
		crate::gql::validate_event_definition(self, &event, scratch, control)?;
		let id = EventId::from_schema_id(self.store.inner.allocate_schema_id()?);
		event.id = Some(id);
		self.working.install_event(event.clone(), self.snapshot.commit_ts)?;
		self.record_event_write(id, bytes);
		self.mark_range_change(ConflictKey::EventCatalogRange);
		self.mutations.push(Mutation::DefineEvent(event));
		Ok(id)
	}

	/// Replaces event source and options while preserving its target and
	/// identity.
	///
	/// # Errors
	///
	/// Returns missing-event, immutable-name/source, syntax, effect, read-only,
	/// capacity or closed-transaction errors. Failure retains the old
	/// definition.
	pub fn replace_event(
		&mut self,
		id: EventId,
		replacement: EventDefinition,
	) -> FerrosGrynnResult<()> {
		self.replace_event_controlled(id, replacement, None)
	}

	/// Shares replacement compilation with the query's existing resource scope.
	pub(crate) fn replace_event_controlled(
		&mut self,
		id: EventId,
		mut replacement: EventDefinition,
		control: Option<&crate::gql::ExecutionControl>,
	) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		let current = self
			.event(id)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("EVENT does not exist"))?;
		if current.name != replacement.name || current.target != replacement.target {
			return Err(FerrosGrynnError::invalid_argument("EVENT name and source are immutable"));
		}
		self.observe_event_source(replacement.target)?;
		let bytes = event_definition_bytes(&replacement)?;
		self.ensure_capacity(bytes)?;
		let scratch = self.event_compiler_scratch(bytes);
		crate::gql::validate_event_definition(self, &replacement, scratch, control)?;
		replacement.id = Some(id);
		replacement.version = current.version.next()?;
		self.working.replace_event(replacement.clone(), self.snapshot.commit_ts)?;
		self.record_event_write(id, bytes);
		self.mark_range_change(ConflictKey::EventCatalogRange);
		self.mutations.push(Mutation::ReplaceEvent(replacement));
		Ok(())
	}

	/// Removes a current definition; accepted asynchronous invocations retain
	/// it.
	///
	/// # Errors
	///
	/// Returns missing-event, read-only, capacity or closed-transaction errors.
	pub fn remove_event(&mut self, id: EventId) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.event(id)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("EVENT does not exist"))?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		self.working.remove_event(id)?;
		self.insert_write(ConflictKey::SchemaEvent(id), SCHEMA_WRITE_BYTES);
		self.mark_range_change(ConflictKey::EventCatalogRange);
		self.mutations.push(Mutation::RemoveEvent(id));
		Ok(())
	}

	/// Observes source existence independently of event namespace lookup.
	fn observe_event_source(&self, target: EventTarget) -> FerrosGrynnResult<()> {
		self.working.validate_event_target(target)?;
		self.record_read(match target {
			EventTarget::Class(class) => ConflictKey::SchemaClass(class),
			EventTarget::Graph(graph) => ConflictKey::GraphDefinition(graph),
		});
		Ok(())
	}

	/// Pays for every retained source mutation independently of FCW key dedup.
	fn record_event_write(&mut self, id: EventId, bytes: usize) {
		self.insert_write(ConflictKey::SchemaEvent(id), 0);
		self.write_bytes += bytes;
	}

	/// Keeps transient compilation inside the remaining transaction admission.
	pub(crate) fn event_compiler_scratch(&self, additional: usize) -> usize {
		self.store
			.inner
			.transaction_limits()
			.max_write_set_bytes
			.min(256 * 1024 * 1024)
			.saturating_sub(self.write_set_bytes())
			.saturating_sub(additional)
	}

	/// Removes current definitions owned by a class or complete graph tree.
	pub(super) fn remove_source_events(&mut self, target: EventTarget) -> FerrosGrynnResult<()> {
		if self.working.schema.events.is_empty() {
			return Ok(());
		}
		self.record_read(ConflictKey::EventCatalogRange);
		let owns = |event: &EventDefinition| {
			event.target == target
				|| match (target, event.target) {
					(EventTarget::Graph(root), EventTarget::Graph(child)) => self
						.working
						.schema
						.graph(child)
						.is_some_and(|graph| graph.owner_root() == Some(root)),
					_ => false,
				}
		};
		let count = self.working.schema.events.values().filter(|event| owns(event)).count();
		let bytes = count
			.checked_mul(SCHEMA_WRITE_BYTES + std::mem::size_of::<EventId>())
			.ok_or_else(|| FerrosGrynnError::operation_overflow("EVENT source removal bytes"))?;
		self.ensure_capacity(bytes)?;
		let ids = self
			.working
			.schema
			.events
			.iter()
			.filter(|(_, event)| owns(event))
			.map(|(id, _)| *id)
			.collect::<Vec<_>>();
		for id in ids {
			self.remove_event(id)?;
		}
		Ok(())
	}
}

/// Accounts retained source and comments before copying into schema/mutations.
fn event_definition_bytes(event: &EventDefinition) -> FerrosGrynnResult<usize> {
	if event
		.comment
		.as_ref()
		.is_some_and(|comment| comment.len() > 4096 || comment.as_str().contains('\0'))
	{
		return Err(FerrosGrynnError::invalid_argument("EVENT comment exceeds its UTF-8 limit"));
	}
	let sources = event
		.name
		.estimated_heap_bytes()
		.checked_add(event.when.estimated_heap_bytes())
		.and_then(|bytes| bytes.checked_add(event.then.estimated_heap_bytes()))
		.and_then(|bytes| {
			bytes.checked_add(
				event.comment.as_ref().map_or(0, crate::values::Text::estimated_heap_bytes),
			)
		})
		.and_then(|bytes| bytes.checked_mul(2))
		.and_then(|bytes| bytes.checked_add(SCHEMA_WRITE_BYTES))
		.ok_or_else(|| FerrosGrynnError::operation_overflow("EVENT definition bytes"))?;
	Ok(sources)
}

#[cfg(test)]
/// Regression tests for event behavior.
mod tests {
	use super::*;
	use crate::{
		datastore::{ClassSchema, Datastore},
		graph::AtomKind,
		values::PropertyMap,
	};

	/// Builds two independent class owners for source-only registration tests.
	fn setup() -> FerrosGrynnResult<(Datastore, EventTarget, EventTarget)> {
		let store = Datastore::in_memory();
		let mut tx = store.begin();
		let item = tx.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
		let audit = tx.define_class(ClassSchema::new("audit", AtomKind::Node, [])?)?;
		tx.commit()?;
		Ok((store, EventTarget::Class(item), EventTarget::Class(audit)))
	}

	/// Verifies event registration compiles but does not execute handler.
	#[test]
	fn event_registration_compiles_but_does_not_execute_handler() {
		use crate::test_assertions::AssertResult as _;

		let (store, target, _audit) = setup().assert_ok();
		let mut tx = store.begin();
		let id = tx
			.define_event(
				EventDefinition::new("audit", target, "true", "{ CREATE audit:unexpected; }")
					.assert_ok(),
			)
			.assert_ok();
		assert!(tx.atom(&"audit:unexpected".parse().assert_ok()).assert_ok().is_none());
		assert_eq!(tx.event(id).assert_ok().map(|event| event.target()), Some(target));
		tx.commit().assert_ok();
		let mut read = store.begin();
		assert!(read.atom(&"audit:unexpected".parse().assert_ok()).assert_ok().is_none());
		assert_eq!(
			read.named_event(target, "audit").assert_ok().and_then(|event| event.id()),
			Some(id)
		);
	}

	/// Verifies event replacement is versioned and failed validation retains
	/// definition.
	#[test]
	fn event_replacement_is_versioned_and_failed_validation_retains_definition() {
		use crate::test_assertions::AssertResult as _;

		let (store, target, _audit) = setup().assert_ok();
		let mut tx = store.begin();
		let id = tx
			.define_event(
				EventDefinition::new("audit", target, "$event = 'CREATE'", "1").assert_ok(),
			)
			.assert_ok();
		tx.commit().assert_ok();
		let mut write = store.begin();
		let before = write
			.event(id)
			.assert_ok()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture event"))
			.assert_ok();
		assert!(
			write
				.replace_event(
					id,
					EventDefinition::new(
						"audit",
						target,
						"true",
						"{ DEFINE CLASS forbidden TYPE NODE; }"
					)
					.assert_ok()
				)
				.is_err()
		);
		assert_eq!(write.event(id).assert_ok().as_deref(), Some(before.as_ref()));
		write
			.replace_event(id, EventDefinition::new("audit", target, "true", "2").assert_ok())
			.assert_ok();
		assert_eq!(write.event(id).assert_ok().map(|event| event.version().get()), Some(2));
		assert_eq!(before.version().get(), 1);
		write.commit().assert_ok();
		let mut remove = store.begin();
		remove.remove_event(id).assert_ok();
		remove.commit().assert_ok();
		assert!(store.begin().event(id).assert_ok().is_none());
	}

	/// Verifies event predicate rejects mutation and source escape without
	/// installation.
	#[test]
	fn event_predicate_rejects_mutation_and_source_escape_without_installation() {
		use crate::test_assertions::AssertResult as _;

		let (store, target, _audit) = setup().assert_ok();
		let mut tx = store.begin();
		for (when, then) in [
			("{ CREATE audit:forbidden; RETURN true; }", "1"),
			("true", "1; CREATE audit:escaped;"),
			("true", "{ COMMIT; }"),
		] {
			assert!(
				tx.define_event(EventDefinition::new("bad", target, when, then).assert_ok())
					.is_err()
			);
			assert!(tx.named_event(target, "bad").assert_ok().is_none());
		}
		assert!(tx.atom(&"audit:forbidden".parse().assert_ok()).assert_ok().is_none());
		assert!(tx.atom(&"audit:escaped".parse().assert_ok()).assert_ok().is_none());
	}

	/// Verifies event absent lookup and replacement use serializable conflicts.
	#[test]
	fn event_absent_lookup_and_replacement_use_serializable_conflicts() {
		use crate::test_assertions::AssertResult as _;

		let (store, target, _audit) = setup().assert_ok();
		let mut observer = store.begin();
		assert!(observer.named_event(target, "new").assert_ok().is_none());
		let mut writer = store.begin();
		writer
			.define_event(EventDefinition::new("new", target, "true", "1").assert_ok())
			.assert_ok();
		writer.commit().assert_ok();
		observer.create_atom("item:observer".parse().assert_ok(), PropertyMap::new()).assert_ok();
		assert!(observer.commit().is_err());
		let mut read = store.begin();
		assert!(read.atom(&"item:observer".parse().assert_ok()).assert_ok().is_none());
	}

	/// Verifies event source removal retires definitions atomically.
	#[test]
	fn event_source_removal_retires_definitions_atomically() {
		use crate::test_assertions::AssertResult as _;

		let (store, target, _audit) = setup().assert_ok();
		let mut tx = store.begin();
		let class_event = tx
			.define_event(EventDefinition::new("class_event", target, "true", "1").assert_ok())
			.assert_ok();
		let graph = tx.define_graph("scope", crate::graph::GraphKind::Graph).assert_ok();
		let graph_event = tx
			.define_event(
				EventDefinition::new("graph_event", EventTarget::Graph(graph.id()), "true", "1")
					.assert_ok(),
			)
			.assert_ok();
		tx.commit().assert_ok();
		let mut remove = store.begin();
		remove.remove_class("item").assert_ok();
		remove.remove_graph("scope").assert_ok();
		assert!(remove.event(class_event).assert_ok().is_none());
		assert!(remove.event(graph_event).assert_ok().is_none());
		remove.commit().assert_ok();
		let mut read = store.begin();
		assert!(read.event(class_event).assert_ok().is_none());
		assert!(read.event(graph_event).assert_ok().is_none());
	}

	/// Verifies event replacements pay for each retained payload.
	#[test]
	fn event_replacements_pay_for_each_retained_payload() {
		use crate::test_assertions::AssertResult as _;

		let (store, target, _audit) = setup().assert_ok();
		let mut tx = store.begin();
		let id = tx
			.define_event(EventDefinition::new("audit", target, "true", "1").assert_ok())
			.assert_ok();
		let first = tx.write_set_bytes();
		let replacement = EventDefinition::new("audit", target, "true", "2")
			.assert_ok()
			.with_comment("x".repeat(1024));
		let bytes = event_definition_bytes(&replacement).assert_ok();
		tx.replace_event(id, replacement.clone()).assert_ok();
		assert_eq!(tx.write_set_bytes(), first + bytes);
		tx.replace_event(id, replacement).assert_ok();
		assert_eq!(tx.write_set_bytes(), first + bytes * 2);
	}

	/// Verifies event transitive compilation respects remaining scratch.
	#[test]
	fn event_transitive_compilation_respects_remaining_scratch() {
		use crate::test_assertions::AssertResult as _;

		let (store, target, _audit) = setup().assert_ok();
		let mut tx = store.begin();
		let source = format!(
			"DEFINE PURE FUNCTION fn::large() -> int {{ /* {} */ RETURN 1; }};",
			"x".repeat(8192)
		);
		for result in tx.execute(&source, &crate::values::Object::new()).assert_ok() {
			let _value = result.result.assert_ok();
		}
		let event = EventDefinition::new("audit", target, "true", "fn::large()").assert_ok();
		assert!(crate::gql::validate_event_definition(&mut tx, &event, 256 * 1024, None).is_err());
		assert!(tx.named_event(target, "audit").assert_ok().is_none());
	}

	/// Verifies event owned child overwrite conflicts with event metadata
	/// reads.
	#[test]
	fn event_owned_child_overwrite_conflicts_with_event_metadata_reads() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let mut setup = store.begin();
		setup.define_class(ClassSchema::new("module", AtomKind::Node, []).assert_ok()).assert_ok();
		setup
			.define_class(ClassSchema::new("document", AtomKind::Node, []).assert_ok())
			.assert_ok();
		let child = setup.define_graph("child", crate::graph::GraphKind::Graph).assert_ok();
		let root = setup.define_graph("root", crate::graph::GraphKind::Metagraph).assert_ok();
		let document = "document:spec".parse::<crate::values::RecordId>().assert_ok();
		let module = "module:owner".parse::<crate::values::RecordId>().assert_ok();
		setup.create_atom(document.clone(), PropertyMap::new()).assert_ok();
		setup.create_atom(module.clone(), PropertyMap::new()).assert_ok();
		setup.attach_node(child.id(), &document, None, None).assert_ok();
		setup.attach_node(root.id(), &module, None, Some(child.id())).assert_ok();
		setup.commit().assert_ok();
		let mut build = store.begin();
		for result in build
			.execute(
				"BUILD GRAPH <META> copied FROM (MATCH (document AS d) WITHIN <(module AS owner)> \
				 IN GRAPH root RETURN d, owner) NODES [d, owner]",
				&crate::values::Object::new(),
			)
			.assert_ok()
		{
			let _value = result.result.assert_ok();
		}
		let copied = build
			.named_graph("copied")
			.assert_ok()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture copied graph"))
			.assert_ok();
		let owned = build
			.working
			.schema
			.all_graphs()
			.find(|graph| graph.owner_root() == Some(copied.id()))
			.map(|graph| graph.id())
			.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture owned graph"))
			.assert_ok();
		let child_event = build
			.define_event(
				EventDefinition::new("child", EventTarget::Graph(owned), "true", "1").assert_ok(),
			)
			.assert_ok();
		let root_event = build
			.define_event(
				EventDefinition::new("root", EventTarget::Graph(copied.id()), "true", "1")
					.assert_ok(),
			)
			.assert_ok();
		build.commit().assert_ok();
		let mut by_id = store.begin();
		let mut by_name = store.begin();
		assert!(by_id.event(child_event).assert_ok().is_some());
		assert!(by_name.named_event(EventTarget::Graph(owned), "child").assert_ok().is_some());
		let mut overwrite = store.begin();
		for result in overwrite
			.execute("BUILD GRAPH OVERWRITE <META> copied FROM []", &crate::values::Object::new())
			.assert_ok()
		{
			let _value = result.result.assert_ok();
		}
		overwrite.commit().assert_ok();
		assert!(store.begin().event(child_event).assert_ok().is_none());
		assert!(store.begin().event(root_event).assert_ok().is_some());
		for (mut observer, record) in [(by_id, "document:by_id"), (by_name, "document:by_name")] {
			observer.create_atom(record.parse().assert_ok(), PropertyMap::new()).assert_ok();
			assert!(observer.commit().is_err());
		}
	}
}

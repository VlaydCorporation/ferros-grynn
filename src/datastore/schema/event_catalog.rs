//! Event catalog replacement uses the same immutable schema root as data
//! writes.

use std::sync::Arc;

use super::{EventDefinition, EventId, EventTarget};
use crate::{
	datastore::DatabaseRoot,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
};

impl DatabaseRoot {
	/// Retires current definitions with their source without affecting job
	/// payloads.
	pub(crate) fn discard_source_events(&mut self, target: EventTarget) {
		if !self.schema.events.values().any(|event| event.target == target) {
			return;
		}
		let schema = Arc::make_mut(&mut self.schema);
		schema.events.retain(|_, event| event.target != target);
		schema.event_names.retain(|(owner, _), _| *owner != target);
	}

	/// Checks the durable source identity before installing a definition.
	pub(crate) fn validate_event_target(&self, target: EventTarget) -> FerrosGrynnResult<()> {
		let present = match target {
			EventTarget::Class(class) => self.schema.class_by_id(class).is_some(),
			EventTarget::Graph(graph) => self.schema.graph(graph).is_some(),
		};
		if !present {
			return Err(FerrosGrynnError::invalid_argument("EVENT source does not exist"));
		}
		Ok(())
	}

	/// Installs one checked event without evaluating its body during replay.
	pub(crate) fn install_event(
		&mut self,
		mut event: EventDefinition,
		published_at: u64,
	) -> FerrosGrynnResult<()> {
		self.validate_event_target(event.target)?;
		let id = event.id.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("a published event requires EventId")
		})?;
		let name = (event.target, event.name.clone());
		if self.schema.events.contains_key(&id) || self.schema.event_names.contains_key(&name) {
			return Err(FerrosGrynnError::invalid_argument("EVENT already exists on its source"));
		}
		event.published_at = published_at;
		let schema = Arc::make_mut(&mut self.schema);
		schema.event_names.insert(name, id);
		schema.events.insert(id, Arc::new(event));
		Ok(())
	}

	/// Replaces a definition while retaining its immutable identity and source.
	pub(crate) fn replace_event(
		&mut self,
		mut event: EventDefinition,
		published_at: u64,
	) -> FerrosGrynnResult<()> {
		self.validate_event_target(event.target)?;
		let id = event.id.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("replacement EVENT requires EventId")
		})?;
		let current = self
			.schema
			.events
			.get(&id)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("EVENT does not exist"))?;
		if current.name != event.name
			|| current.target != event.target
			|| event.version != current.version.next()?
		{
			return Err(FerrosGrynnError::invalid_argument(
				"EVENT replacement changed identity/source or skipped a version",
			));
		}
		event.published_at = published_at;
		Arc::make_mut(&mut self.schema).events.insert(id, Arc::new(event));
		Ok(())
	}

	/// Removes a current definition without touching already accepted jobs.
	pub(crate) fn remove_event(&mut self, id: EventId) -> FerrosGrynnResult<()> {
		let current = self
			.schema
			.events
			.get(&id)
			.cloned()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("EVENT does not exist"))?;
		let schema = Arc::make_mut(&mut self.schema);
		schema.event_names.remove(&(current.target, current.name.clone()));
		schema.events.remove(&id);
		Ok(())
	}
}

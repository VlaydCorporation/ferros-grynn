//! Versioned event definitions shared by the SDK, analyzer and durable catalog.

use std::fmt::{Display, Formatter};

use super::{ClassId, SchemaId, SchemaVersion};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::GraphId,
	values::Text,
};

/// Maximum retained UTF-8 source bytes for either event expression.
const MAX_EVENT_SOURCE_BYTES: usize = 64 * 1024;

/// Stable identity of one event definition from the common schema clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct EventId(SchemaId);

impl EventId {
	/// Creates an identity, rejecting the reserved zero and maximum values.
	pub const fn new(value: u32) -> Option<Self> {
		match SchemaId::new(value) {
			Some(value) => Some(Self(value)),
			None => None,
		}
	}

	/// Returns the durable integer identity.
	pub const fn get(self) -> u32 {
		self.0.get()
	}

	/// Wraps an identity allocated by the common schema clock.
	pub(crate) const fn from_schema_id(value: SchemaId) -> Self {
		Self(value)
	}

	/// Validates an identity at the durable boundary.
	pub(crate) fn from_storage(value: u32) -> FerrosGrynnResult<Self> {
		Self::new(value)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("event identity is reserved"))
	}
}

impl From<EventId> for SchemaId {
	fn from(value: EventId) -> Self {
		value.0
	}
}

impl Display for EventId {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		Display::fmt(&self.get(), f)
	}
}

/// Stable event owner; display names do not identify a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum EventTarget {
	/// Database-global class records.
	Class(ClassId),
	/// Qualified occurrences and their effective values.
	Graph(GraphId),
}

/// Checked retry and causal-depth policy of an asynchronous event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventAsyncOptions {
	/// Additional attempts after the initial execution.
	retries: u8,
	/// Inclusive causal-depth ceiling inherited by child invocations.
	max_depth: u8,
}

impl Default for EventAsyncOptions {
	fn default() -> Self {
		Self {
			retries: 0,
			max_depth: 3,
		}
	}
}

impl EventAsyncOptions {
	/// Checks a retry count and a maximum causal depth in `0..=16`.
	///
	/// # Errors
	///
	/// Returns an invalid-argument status when the depth exceeds sixteen.
	pub fn new(retries: u8, max_depth: u8) -> FerrosGrynnResult<Self> {
		if max_depth > 16 {
			return Err(FerrosGrynnError::invalid_argument("EVENT MAXDEPTH must be in 0..=16"));
		}
		Ok(Self {
			retries,
			max_depth,
		})
	}

	/// Returns the number of attempts after the initial attempt.
	pub const fn retries(self) -> u8 {
		self.retries
	}

	/// Returns the inclusive causal-depth ceiling.
	pub const fn max_depth(self) -> u8 {
		self.max_depth
	}
}

/// Transactional synchronous execution or durable asynchronous execution.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EventExecution {
	/// Execute after mutation within its transaction and rollback boundary.
	#[default]
	Sync,
	/// Enqueue atomically with the mutation and execute in a fresh transaction.
	Async(EventAsyncOptions),
}

/// Owned source definition; executable typed IR is compiled independently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventDefinition {
	/// Stable identity assigned on transactional registration.
	pub(crate) id: Option<EventId>,
	/// Checked semantic source and policy revision.
	pub(crate) version: SchemaVersion,
	/// Commit publishing this definition revision.
	pub(crate) published_at: u64,
	/// Name within the stable source owner.
	pub(crate) name: Text,
	/// Class or graph identity, independent of its display name.
	pub(crate) target: EventTarget,
	/// Synchronous execution or checked durable asynchronous policy.
	pub(crate) execution: EventExecution,
	/// Owned read-only predicate source.
	pub(crate) when: Text,
	/// Owned handler expression or block source.
	pub(crate) then: Text,
	/// Optional durable source comment.
	pub(crate) comment: Option<Text>,
}

impl EventDefinition {
	/// Returns the source byte ceiling shared with durable codecs.
	pub(crate) const fn source_byte_limit() -> usize {
		MAX_EVENT_SOURCE_BYTES
	}

	/// Creates an unpublished synchronous event with owned source expressions.
	/// Source syntax, effects and owner existence are validated on
	/// registration.
	///
	/// # Errors
	///
	/// Returns an invalid-argument status for an invalid name, empty source,
	/// NUL bytes, or source larger than 64 KiB.
	pub fn new<Name: Into<Text>, When: Into<Text>, Then: Into<Text>>(
		name: Name,
		target: EventTarget,
		when: When,
		then: Then,
	) -> FerrosGrynnResult<Self> {
		let name = name.into();
		let when = when.into();
		let then = then.into();
		if name.is_empty() || name.len() > 4096 || name.as_str().contains('\0') {
			return Err(FerrosGrynnError::invalid_argument(
				"EVENT name must contain 1..=4096 non-NUL UTF-8 bytes",
			));
		}
		for source in [&when, &then] {
			if source.is_empty()
				|| source.len() > MAX_EVENT_SOURCE_BYTES
				|| source.as_str().contains('\0')
			{
				return Err(FerrosGrynnError::invalid_argument(
					"EVENT source must contain 1..=65536 non-NUL UTF-8 bytes",
				));
			}
		}
		Ok(Self {
			id: None,
			version: SchemaVersion::INITIAL,
			published_at: 0,
			name,
			target,
			execution: EventExecution::Sync,
			when,
			then,
			comment: None,
		})
	}

	/// Selects synchronous execution or a checked asynchronous policy.
	#[must_use]
	pub const fn with_execution(mut self, execution: EventExecution) -> Self {
		self.execution = execution;
		self
	}

	/// Attaches an optional source comment, validated by catalog publication.
	#[must_use]
	pub fn with_comment<Comment: Into<Text>>(mut self, comment: Comment) -> Self {
		self.comment = Some(comment.into());
		self
	}

	/// Returns the stable identity after transactional registration.
	pub const fn id(&self) -> Option<EventId> {
		self.id
	}

	/// Returns the definition version.
	pub const fn version(&self) -> SchemaVersion {
		self.version
	}

	/// Returns the event name within its source owner.
	pub const fn name(&self) -> &Text {
		&self.name
	}

	/// Returns the stable owning class or graph.
	pub const fn target(&self) -> EventTarget {
		self.target
	}

	/// Returns the execution policy.
	pub const fn execution(&self) -> EventExecution {
		self.execution
	}

	/// Returns the retained read-only predicate source.
	pub const fn when(&self) -> &Text {
		&self.when
	}

	/// Returns the retained handler source.
	pub const fn then(&self) -> &Text {
		&self.then
	}

	/// Returns the optional durable comment.
	pub const fn comment(&self) -> Option<&Text> {
		self.comment.as_ref()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn event_policy_checks_reserved_identities_and_depth() -> FerrosGrynnResult<()> {
		assert!(EventId::new(0).is_none());
		assert!(EventId::new(u32::MAX).is_none());
		assert_eq!(EventId::new(1).map(EventId::get), Some(1));
		assert_eq!(EventAsyncOptions::default().max_depth(), 3);
		assert_eq!(EventAsyncOptions::default().retries(), 0);
		assert_eq!(EventAsyncOptions::new(255, 0)?.retries(), 255);
		assert_eq!(EventAsyncOptions::new(0, 16)?.max_depth(), 16);
		assert!(EventAsyncOptions::new(0, 17).is_err());
		Ok(())
	}

	#[test]
	fn event_source_is_owned_and_bounded() -> FerrosGrynnResult<()> {
		let graph = GraphId::new(1)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture graph identity"))?;
		let target = EventTarget::Graph(graph);
		let definition = EventDefinition::new("audit", target, "true", "{ RETURN 1; }")?;
		assert_eq!(definition.id(), None);
		assert_eq!(definition.execution(), EventExecution::Sync);
		assert_eq!(definition.target(), target);
		assert!(EventDefinition::new("", target, "true", "1").is_err());
		assert!(EventDefinition::new("audit", target, "", "1").is_err());
		assert!(EventDefinition::new("audit", target, "true", "\0").is_err());
		assert!(EventDefinition::new("audit", target, "true", "x".repeat(65_537)).is_err());
		let longest = EventDefinition::new("audit", target, "true", "x".repeat(65_536))?;
		assert_eq!(longest.then().len(), 65_536);
		Ok(())
	}
}

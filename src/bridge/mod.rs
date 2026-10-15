//! Owned database bridges, mapping identities and asynchronous boundary ports.
//!
//! Both directions retain typed values. Clients are attached at runtime;
//! durable references retain only logical identities. A Tokio runtime with
//! time enabled runs bridge IO while the existing executor runs on its blocking
//! pool. External mirror failures remain visible in
//! [`crate::bridge::BridgeQueryResult::effects`].
#![doc = include_str!("../../docs/rustdoc/bridge-example-01.md")]

mod ferros;
mod query;
#[cfg(feature = "bridge-surreal")]
mod surreal;
pub(crate) mod types;
pub use ferros::FerrosBridge;
pub(crate) use query::BridgeQueryScope;
pub use query::{BridgeEffectOutcome, BridgeQuery, BridgeQueryResult, FerrosQueryBridgeExt};
#[cfg(feature = "bridge-surreal")]
pub use surreal::{SurrealBridge, SurrealBridgedQuery, SurrealQueryBridgeExt};
#[cfg(test)]
mod operator_tests;
#[cfg(test)]
mod tests;

use std::{future::Future, pin::Pin};

pub use types::{
	BridgeDatabase,
	BridgeDatabaseId,
	BridgeDatabaseOptions,
	BridgeDatabaseRef,
	BridgeEntityRef,
	BridgeMapping,
	BridgeMappingId,
	BridgeMappingRef,
	BridgeMappingRole,
	BridgeMappingState,
	BridgeMappingSubject,
	BridgeMirrorRequest,
};

use crate::{
	gql_status::FerrosGrynnResult,
	values::{Object, Text, Value},
};

/// Object-safe asynchronous operation using only Ferros-Grynn owned values.
pub type BridgeFuture<'a> = Pin<Box<dyn Future<Output = FerrosGrynnResult<Value>> + Send + 'a>>;

/// One actual bridge operation; adapters reject unsupported capabilities.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum BridgeOperation {
	/// A native query whose text belongs to the selected adapter.
	Query {
		/// Adapter-native source text.
		source: Text,
		/// Typed source query parameters.
		variables: Object,
	},
	/// A record or class read, without importing local atoms.
	RecordOrClass(
		/// A record or class read, without importing local atoms.
		Text,
	),
	/// Exact external records requested by the shared FETCH walker.
	Records(
		/// Exact external records requested by the shared FETCH walker.
		Vec<crate::values::RecordId>,
	),
	/// Idempotent relation creation or verification using a reserved identity.
	Mirror(
		/// Idempotent relation creation or verification using a reserved
		/// identity.
		BridgeMirrorRequest,
	),
}

/// Open port implemented by application adapters and the supplied bridges.
pub trait Bridge: Send + Sync {
	/// Named adapters identify the durable descriptor they were configured for.
	/// Query-only sources return None and cannot form a datastore ownership
	/// cycle.
	fn database(&self) -> Option<BridgeDatabaseRef> {
		None
	}
	/// Declares supported operations before any local mutation is staged.
	fn capabilities(&self) -> BridgeCapabilities {
		BridgeCapabilities::READ_ONLY
	}
	/// Executes one owned operation without exposing driver or storage types.
	fn execute(&self, operation: BridgeOperation, context: BridgeContext) -> BridgeFuture<'_>;
}

bitflags::bitflags! {
	/// Independent capabilities required by bridge operations.
	#[derive(Debug, Clone, Copy, PartialEq, Eq)]
	pub struct BridgeCapabilities: u8 {
		/// Native query and record/class sources are available.
		const QUERIES = 0b0001;
		/// Source queries obey the Ferros-Grynn read-only effect ceiling.
		const INTERNAL_QUERIES = 0b0010;
		/// Exact property-object batch reads are available.
		const RECORDS = 0b0100;
		/// Idempotent relation verification/creation is available.
		const MIRROR = 0b1000;
	}
}

impl BridgeCapabilities {
	/// Full embedded adapter including checked idempotent relation mirroring.
	pub const FULL: Self = Self::QUERIES.union(Self::RECORDS).union(Self::MIRROR);
	/// Query-only adapters do not implicitly accept writes.
	pub const READ_ONLY: Self = Self::QUERIES;

	/// Reports whether native query and record/class sources are supported.
	pub const fn queries(self) -> bool {
		self.contains(Self::QUERIES)
	}

	/// Reports whether source queries enforce the read-only GQL effect ceiling.
	pub const fn internal_queries(self) -> bool {
		self.contains(Self::INTERNAL_QUERIES)
	}

	/// Reports whether exact property-object batch reads are supported.
	pub const fn records(self) -> bool {
		self.contains(Self::RECORDS)
	}

	/// Reports whether idempotent relation mirroring is supported.
	pub const fn mirror(self) -> bool {
		self.contains(Self::MIRROR)
	}
}

impl<B: Bridge + ?Sized> Bridge for std::sync::Arc<B> {
	/// Returns database for `std::sync::Arc<B>`.
	fn database(&self) -> Option<BridgeDatabaseRef> {
		self.as_ref().database()
	}

	/// Returns capabilities for `std::sync::Arc<B>`.
	fn capabilities(&self) -> BridgeCapabilities {
		self.as_ref().capabilities()
	}

	/// Forwards the operation and execution context to the wrapped bridge.
	fn execute(&self, operation: BridgeOperation, context: BridgeContext) -> BridgeFuture<'_> {
		self.as_ref().execute(operation, context)
	}
}

/// Shared interruption and memory scope; it retains no storage snapshot.
#[derive(Debug, Clone)]
pub struct BridgeContext {
	/// Caller-owned execution and resource limits.
	pub(crate) options: crate::gql::QueryExecutionOptions,
	/// Query-owned cancellation token, also set when orchestration drops.
	pub(crate) cancellation: crate::gql::QueryCancellation,
	/// Optional caller-owned cancellation authority.
	pub(crate) parent_cancellation: Option<crate::gql::QueryCancellation>,
	/// Absolute deadline retained across IO and commit barriers.
	pub(crate) deadline: Option<std::time::Instant>,
}
impl BridgeContext {
	/// Returns bounded source-execution options owned by this operation.
	pub fn options(&self) -> crate::gql::QueryExecutionOptions {
		let mut options = self.options.clone();
		if let Some(deadline) = self.deadline {
			options.statement_timeout =
				Some(deadline.saturating_duration_since(std::time::Instant::now()));
		}
		options
	}

	/// Checks the original deadline and both interruption authorities.
	/// # Errors
	/// Returns the common query cancellation or timeout status.
	pub fn check(&self) -> crate::gql_status::FerrosGrynnResult<()> {
		if self.cancellation.is_cancelled()
			|| self
				.parent_cancellation
				.as_ref()
				.is_some_and(crate::gql::QueryCancellation::is_cancelled)
		{
			return Err(crate::gql_status::FerrosGrynnError::query_cancelled());
		}
		if self.deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
			return Err(crate::gql_status::FerrosGrynnError::query_timed_out(0));
		}
		Ok(())
	}

	/// Admits an owned adapter value into the same receiving statement pool.
	/// # Errors
	/// Returns the common resource-limit status before retained copies are
	/// made.
	pub fn admit_value(&self, value: &Value) -> crate::gql_status::FerrosGrynnResult<()> {
		check_value_shape(
			value,
			0,
			&mut (self.options.barrier_memory_bytes.get() / std::mem::size_of::<Value>() + 1),
			self,
		)?;
		self.admit_bytes(
			value.estimated_heap_bytes().checked_add(std::mem::size_of::<Value>()).ok_or_else(
				|| {
					crate::gql_status::FerrosGrynnError::operation_overflow(
						"bridge value admission",
					)
				},
			)?,
		)
	}

	/// Admits boundary metadata before allocating or cloning it.
	/// # Errors
	/// Returns overflow, cancellation, timeout or the common resource status.
	pub fn admit_bytes(&self, bytes: usize) -> crate::gql_status::FerrosGrynnResult<()> {
		self.check()?;
		if let Some(account) = &self.options.context.bridge_memory
			&& !account.lock().reserve(bytes)
		{
			return Err(crate::gql_status::FerrosGrynnError::traversal_resource_limit_exceeded(
				"bridge retained values",
				self.options.barrier_memory_bytes.get() as u64,
			));
		}
		Ok(())
	}
}

/// Bounds recursive collection processing before heap estimation or copying.
fn check_value_shape(
	value: &Value,
	depth: usize,
	nodes: &mut usize,
	context: &BridgeContext,
) -> crate::gql_status::FerrosGrynnResult<()> {
	context.check()?;
	if depth > 64 || *nodes == 0 {
		return Err(crate::gql_status::FerrosGrynnError::bridge_operation_error(
			"bridge value exceeds nesting or node admission",
		));
	}
	*nodes -= 1;
	match value {
		Value::Array(values) => {
			for value in values {
				check_value_shape(value, depth + 1, nodes, context)?;
			}
		}
		Value::Set(values) => {
			for value in values {
				check_value_shape(value, depth + 1, nodes, context)?;
			}
		}
		Value::Object(values) => {
			for value in values.values() {
				check_value_shape(value, depth + 1, nodes, context)?;
			}
		}
		Value::Range(range) => {
			for bound in [range.start(), range.end()] {
				if let std::ops::Bound::Included(value) | std::ops::Bound::Excluded(value) = bound {
					check_value_shape(value, depth + 1, nodes, context)?;
				}
			}
		}
		Value::Geometry(value) => {
			value.validation_scratch_bytes()?;
		}
		_ => {}
	}
	Ok(())
}

/// Borrowed query-source descriptions retained as owned data by builders.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum BridgeData<'a> {
	/// Native class/record spelling and the receiving parameter name.
	RecordOrClass(
		/// Native record or class spelling interpreted by the selected bridge.
		&'a str,
		/// Name of the query parameter receiving the selected external data.
		&'a str,
	),
	/// External native query and optional Object-field to parameter mapping.
	ExternalQuery(
		/// Query text evaluated by the external database through its bridge.
		&'a str,
		/// Optional mapping from external object field names to query parameter
		/// names.
		Option<std::collections::HashMap<&'a str, &'a str>>,
	),
	/// Read-only GQL query and optional Object-field to parameter mapping.
	InternalQuery(
		/// Read-only GrynnQL source evaluated against the local datastore.
		&'a str,
		/// Optional mapping from local result object fields to query parameter
		/// names.
		Option<std::collections::HashMap<&'a str, &'a str>>,
	),
}

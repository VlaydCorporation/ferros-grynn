//! Owned database bridges, mapping identities and asynchronous boundary ports.
//!
//! Both directions retain typed values. Clients are attached at runtime;
//! durable references retain only logical identities. A Tokio runtime with
//! time enabled runs bridge IO while the existing executor runs on its blocking
//! pool. External mirror failures remain visible in
//! [`BridgeQueryResult::effects`].
//!
//! ```
//! use ferros_grynn::{
//! 	bridge::{
//! 		Bridge,
//! 		BridgeContext,
//! 		BridgeData,
//! 		BridgeFuture,
//! 		BridgeOperation,
//! 		FerrosQueryBridgeExt,
//! 	},
//! 	datastore::Datastore,
//! 	gql_status::FerrosGrynnResult,
//! 	values::Value,
//! };
//! struct Source;
//! impl Bridge for Source {
//! 	fn execute(&self, _: BridgeOperation, context: BridgeContext) -> BridgeFuture<'_> {
//! 		Box::pin(async move {
//! 			context.check()?;
//! 			Ok(Value::Null)
//! 		})
//! 	}
//! }
//! # fn main() -> FerrosGrynnResult<()> {
//! let runtime =
//! 	tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| {
//! 		ferros_grynn::gql_status::FerrosGrynnError::bridge_operation_error(
//! 			"example runtime failed",
//! 		)
//! 		.with_technical_source(error)
//! 	})?;
//! let store = Datastore::in_memory();
//! let query = FerrosQueryBridgeExt::use_bridge(
//! 	store.query("RETURN $bridge"),
//! 	Source,
//! 	BridgeData::ExternalQuery("native source", None),
//! );
//! let result = runtime.block_on(query.execute())?;
//! assert_eq!(result.results[0].result.clone()?.into_parts().0, Value::Null);
//! # Ok(()) }
//! ```

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
	RecordOrClass(Text),
	/// Exact external records requested by the shared FETCH walker.
	Records(Vec<crate::values::RecordId>),
	/// Idempotent relation creation or verification using a reserved identity.
	Mirror(BridgeMirrorRequest),
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

/// Concrete boundary capabilities required by actual GQL operations.
#[derive(Debug, Clone, Copy)]
pub struct BridgeCapabilities {
	/// Native query and record/class sources are available.
	pub queries: bool,
	/// Source queries obey the Ferros-Grynn read-only effect ceiling.
	pub internal_queries: bool,
	/// Exact property-object batch reads are available.
	pub records: bool,
	/// Idempotent relation verification/creation is available.
	pub mirror: bool,
}
impl BridgeCapabilities {
	/// Full embedded adapter including checked idempotent relation mirroring.
	pub const FULL: Self = Self {
		queries: true,
		internal_queries: false,
		records: true,
		mirror: true,
	};
	/// Query-only adapters do not implicitly accept writes.
	pub const READ_ONLY: Self = Self {
		queries: true,
		internal_queries: false,
		records: false,
		mirror: false,
	};
}

impl<B: Bridge + ?Sized> Bridge for std::sync::Arc<B> {
	fn database(&self) -> Option<BridgeDatabaseRef> {
		self.as_ref().database()
	}

	fn capabilities(&self) -> BridgeCapabilities {
		self.as_ref().capabilities()
	}

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
		if let Some(account) = &self.options.bridge_memory {
			if !account.lock().reserve(bytes) {
				return Err(
					crate::gql_status::FerrosGrynnError::traversal_resource_limit_exceeded(
						"bridge retained values",
						self.options.barrier_memory_bytes.get() as u64,
					),
				);
			}
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
	RecordOrClass(&'a str, &'a str),
	/// External native query and optional Object-field to parameter mapping.
	ExternalQuery(&'a str, Option<std::collections::HashMap<&'a str, &'a str>>),
	/// Read-only GQL query and optional Object-field to parameter mapping.
	InternalQuery(&'a str, Option<std::collections::HashMap<&'a str, &'a str>>),
}

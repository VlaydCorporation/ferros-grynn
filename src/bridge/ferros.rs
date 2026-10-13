//! Read-only GQL source adapter; it never promotes transaction access.

use super::{Bridge, BridgeCapabilities, BridgeContext, BridgeFuture, BridgeOperation};
use crate::{datastore::Datastore, gql_status::FerrosGrynnError};

/// Source-side adapter supplying owned GQL data to another database.
pub struct FerrosBridge {
	/// Datastore retained for the lifetime of this operation.
	store: Datastore,
}

impl FerrosBridge {
	/// Retains the datastore while a source operation is active.
	pub const fn new(store: Datastore) -> Self {
		Self {
			store,
		}
	}
}

impl Bridge for FerrosBridge {
	fn capabilities(&self) -> BridgeCapabilities {
		BridgeCapabilities {
			internal_queries: true,
			..BridgeCapabilities::READ_ONLY
		}
	}

	fn execute(&self, operation: BridgeOperation, context: BridgeContext) -> BridgeFuture<'_> {
		Box::pin(async move {
			context.check()?;
			let BridgeOperation::Query {
				source,
				variables,
			} = operation
			else {
				return Err(FerrosGrynnError::bridge_operation_error(
					"FerrosBridge accepts read-only InternalQuery sources",
				));
			};
			super::query::internal_query(self.store.clone(), source, variables, context).await
		})
	}
}

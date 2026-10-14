//! Public integration contracts, grouped by subsystem and storage backend.
#![allow(
	clippy::unwrap_used,
	reason = "integration assertions stop immediately on unexpected public API errors"
)]

#[cfg(test)]
#[path = "system/support.rs"]
mod support;

#[cfg(test)]
#[cfg(feature = "columnar")]
#[path = "system/columnar/mod.rs"]
mod columnar;

#[cfg(test)]
#[path = "system/indexes/mod.rs"]
mod indexes;
#[cfg(test)]
#[path = "system/operations/mod.rs"]
mod operations;
#[cfg(test)]
#[path = "system/queries/mod.rs"]
mod queries;
#[cfg(test)]
#[path = "system/schema/mod.rs"]
mod schema;
#[cfg(test)]
#[path = "system/sdk/mod.rs"]
mod sdk;
#[cfg(test)]
#[path = "system/transactions/mod.rs"]
mod transactions;

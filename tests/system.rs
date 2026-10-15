#![cfg(test)]

//! Public integration contracts, grouped by subsystem and storage backend.
#![allow(
	clippy::unwrap_used,
	reason = "integration assertions stop immediately on unexpected public API errors"
)]
/// Assertions shared with the root unit-test harness.
#[path = "common/assertions.rs"]
pub mod test_assertions;

#[cfg(test)]
#[path = "system/support.rs"]
/// Fixtures and helpers shared by public system tests across storage backends.
mod support;

#[cfg(test)]
#[cfg(feature = "columnar")]
#[path = "system/columnar/mod.rs"]
/// Columnar presence and value conversion across storage backends.
mod columnar;

#[cfg(test)]
#[path = "system/indexes/mod.rs"]
/// Public index behavior across storage backends.
mod indexes;
#[cfg(test)]
#[path = "system/operations/mod.rs"]
/// Public atom and graph operations across storage backends.
mod operations;
#[cfg(test)]
#[path = "system/queries/mod.rs"]
/// Public query execution across storage backends.
mod queries;
#[cfg(test)]
#[path = "system/schema/mod.rs"]
/// Public schema definitions across storage backends.
mod schema;
#[cfg(test)]
#[path = "system/sdk/mod.rs"]
/// Public SDK contracts across storage backends.
mod sdk;
#[cfg(test)]
#[path = "system/transactions/mod.rs"]
/// Transaction isolation and publication across storage backends.
mod transactions;

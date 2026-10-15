#![cfg(test)]

//! Caller-side selector expansion uses the existing default Tokio feature
//! graph.
#![cfg(feature = "datafusion")]
#[cfg(test)]
use crate::test_assertions::AssertResult as _;

/// Assertions shared with the root unit-test harness.
#[path = "common/assertions.rs"]
pub mod test_assertions;

use ferros_grynn as fg;
use fg::{gql_status::FerrosGrynnError, live::LiveGraphNext};

/// Verifies graph selector supports external branches and renamed crate.
#[test]
fn graph_selector_supports_external_branches_and_renamed_crate() {
	let runtime = tokio::runtime::Builder::new_current_thread().build().assert_ok();
	let result = runtime.block_on(async {
		fg::live_graph_select! {
			select = tokio::select;
			event = std::future::ready(Ok::<_, FerrosGrynnError>(LiveGraphNext::Updated { revision: 7.into() })) => {
				Ok(LiveGraphNext::Updated { revision }) => revision.get(),
				Ok(_) => 0,
				Err(_) => 1,
			},
			_ = std::future::pending::<()>() => 99,
		}
	});
	assert_eq!(result, 7);
	let cancelled = runtime.block_on(async {
		fg::live_graph_select! {
			select = tokio::select;
			event = std::future::pending::<Result<LiveGraphNext, FerrosGrynnError>>() => {
				_ => false,
			},
			_ = std::future::ready(()) => true,
		}
	});
	assert!(cancelled);
}

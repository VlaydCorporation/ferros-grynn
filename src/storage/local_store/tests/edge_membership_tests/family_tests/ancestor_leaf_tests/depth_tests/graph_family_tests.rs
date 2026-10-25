//! General Graph ancestors retain full scopes without rotating the physical
//! generation.

use super::*;

/// Direct Graph ancestors R and S preserve M leaf replacement.
#[test]
fn ancestor_family_sync_direct_node_replacement_retains_files_and_reopens() {
	replace_depth_with_families(IoMode::Sync, AtomKind::Node, false, [
		GraphKind::Graph,
		GraphKind::Graph,
		GraphKind::Metagraph,
	]);
}

/// Direct Graph ancestors R and S preserve M leaf replacement.
#[test]
fn ancestor_family_sync_direct_edge_replacement_retains_files_and_reopens() {
	replace_depth_with_families(IoMode::Sync, AtomKind::Edge, false, [
		GraphKind::Graph,
		GraphKind::Graph,
		GraphKind::Metagraph,
	]);
}

/// Direct Graph ancestors R and S preserve M leaf replacement.
#[cfg(feature = "native-async-io")]
#[test]
fn ancestor_family_native_async_direct_node_replacement_retains_files_and_reopens() {
	replace_depth_with_families(IoMode::NativeAsync, AtomKind::Node, false, [
		GraphKind::Graph,
		GraphKind::Graph,
		GraphKind::Metagraph,
	]);
}

/// Direct Graph ancestors R and S preserve M leaf replacement.
#[cfg(feature = "native-async-io")]
#[test]
fn ancestor_family_native_async_direct_edge_replacement_retains_files_and_reopens() {
	replace_depth_with_families(IoMode::NativeAsync, AtomKind::Edge, false, [
		GraphKind::Graph,
		GraphKind::Graph,
		GraphKind::Metagraph,
	]);
}

/// A Graph T preserves every exact two-hop full scope.
#[test]
fn ancestor_family_sync_top_node_replacement_retains_files_and_reopens() {
	replace_depth_with_families(IoMode::Sync, AtomKind::Node, false, [
		GraphKind::Metagraph,
		GraphKind::Metagraph,
		GraphKind::Graph,
	]);
}

/// A Graph T preserves every exact two-hop full scope.
#[test]
fn ancestor_family_sync_top_edge_replacement_retains_files_and_reopens() {
	replace_depth_with_families(IoMode::Sync, AtomKind::Edge, false, [
		GraphKind::Metagraph,
		GraphKind::Metagraph,
		GraphKind::Graph,
	]);
}

/// A Graph T preserves every exact two-hop full scope.
#[cfg(feature = "native-async-io")]
#[test]
fn ancestor_family_native_async_top_node_replacement_retains_files_and_reopens() {
	replace_depth_with_families(IoMode::NativeAsync, AtomKind::Node, false, [
		GraphKind::Metagraph,
		GraphKind::Metagraph,
		GraphKind::Graph,
	]);
}

/// A Graph T preserves every exact two-hop full scope.
#[cfg(feature = "native-async-io")]
#[test]
fn ancestor_family_native_async_top_edge_replacement_retains_files_and_reopens() {
	replace_depth_with_families(IoMode::NativeAsync, AtomKind::Edge, false, [
		GraphKind::Metagraph,
		GraphKind::Metagraph,
		GraphKind::Graph,
	]);
}

/// Graph R/S/T owners publish the same canonical bundle.
#[test]
fn ancestor_family_sync_all_node_replacement_retains_files_and_reopens() {
	replace_depth_with_families(IoMode::Sync, AtomKind::Node, false, [GraphKind::Graph; 3]);
}

/// Graph R/S/T owners publish the same canonical bundle.
#[test]
fn ancestor_family_sync_all_edge_replacement_retains_files_and_reopens() {
	replace_depth_with_families(IoMode::Sync, AtomKind::Edge, false, [GraphKind::Graph; 3]);
}

/// Graph R/S/T owners publish the same canonical bundle.
#[cfg(feature = "native-async-io")]
#[test]
fn ancestor_family_native_async_all_node_replacement_retains_files_and_reopens() {
	replace_depth_with_families(IoMode::NativeAsync, AtomKind::Node, false, [GraphKind::Graph; 3]);
}

/// Graph R/S/T owners publish the same canonical bundle.
#[cfg(feature = "native-async-io")]
#[test]
fn ancestor_family_native_async_all_edge_replacement_retains_files_and_reopens() {
	replace_depth_with_families(IoMode::NativeAsync, AtomKind::Edge, false, [GraphKind::Graph; 3]);
}

/// Qualified terminal retargeting remains invalid across Graph ancestors.
#[test]
fn ancestor_family_sync_unanchored_replacement_publishes_nothing() {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		replace_depth_with_families(IoMode::Sync, kind, true, [GraphKind::Graph; 3]);
	}
}

/// Qualified terminal retargeting remains invalid across Graph ancestors.
#[cfg(feature = "native-async-io")]
#[test]
fn ancestor_family_native_async_unanchored_replacement_publishes_nothing() {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		replace_depth_with_families(IoMode::NativeAsync, kind, true, [GraphKind::Graph; 3]);
	}
}

/// Graph T rejects foreign cache owners and wrong physical container slots.
#[test]
fn ancestor_family_sync_false_sources_preserves_complete_arenas() {
	source_tests::bad_top_sources_with_families(IoMode::Sync, [GraphKind::Graph; 3]);
}

/// The opened maintenance envelope admits or rejects the whole Graph bundle.
#[test]
fn ancestor_family_sync_resource_rejection_preserves_complete_arenas() {
	resource_tests::reject_with_families(IoMode::Sync, [GraphKind::Graph; 3]);
}

/// Graph owners recover the complete old or final durable catalog decision.
#[test]
fn ancestor_family_sync_failure_recovery_preserves_complete_arenas() {
	recovery_tests::failures_with_families(IoMode::Sync, [GraphKind::Graph; 3]);
}

/// Graph T rejects foreign cache owners and wrong physical container slots.
#[cfg(feature = "native-async-io")]
#[test]
fn ancestor_family_native_async_false_sources_preserves_complete_arenas() {
	source_tests::bad_top_sources_with_families(IoMode::NativeAsync, [GraphKind::Graph; 3]);
}

/// The opened maintenance envelope admits or rejects the whole Graph bundle.
#[cfg(feature = "native-async-io")]
#[test]
fn ancestor_family_native_async_resource_rejection_preserves_complete_arenas() {
	resource_tests::reject_with_families(IoMode::NativeAsync, [GraphKind::Graph; 3]);
}

/// Graph owners recover the complete old or final durable catalog decision.
#[cfg(feature = "native-async-io")]
#[test]
fn ancestor_family_native_async_failure_recovery_preserves_complete_arenas() {
	recovery_tests::failures_with_families(IoMode::NativeAsync, [GraphKind::Graph; 3]);
}

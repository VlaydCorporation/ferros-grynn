//! Graph M preserves complete ancestor paths and old physical pins.

use super::*;

/// Graph M retains every file owner, old pin and final cold path.
#[test]
fn graph_owner_sync_node_metagraph_ancestors_retains_all_owners() {
	replace_depth_with_parent(
		IoMode::Sync,
		AtomKind::Node,
		false,
		[GraphKind::Metagraph; 3],
		GraphKind::Graph,
	);
}

/// Graph M retains every file owner, old pin and final cold path.
#[test]
fn graph_owner_sync_edge_metagraph_ancestors_retains_all_owners() {
	replace_depth_with_parent(
		IoMode::Sync,
		AtomKind::Edge,
		false,
		[GraphKind::Metagraph; 3],
		GraphKind::Graph,
	);
}

/// Graph M retains every file owner, old pin and final cold path.
#[test]
fn graph_owner_sync_node_graph_ancestors_retains_all_owners() {
	replace_depth_with_parent(
		IoMode::Sync,
		AtomKind::Node,
		false,
		[GraphKind::Graph; 3],
		GraphKind::Graph,
	);
}

/// Graph M retains every file owner, old pin and final cold path.
#[test]
fn graph_owner_sync_edge_graph_ancestors_retains_all_owners() {
	replace_depth_with_parent(
		IoMode::Sync,
		AtomKind::Edge,
		false,
		[GraphKind::Graph; 3],
		GraphKind::Graph,
	);
}

/// Graph M retains every file owner, old pin and final cold path.
#[test]
fn graph_owner_sync_node_mixed_ancestors_retains_all_owners() {
	replace_depth_with_parent(
		IoMode::Sync,
		AtomKind::Node,
		false,
		[GraphKind::Graph, GraphKind::Metagraph, GraphKind::Graph],
		GraphKind::Graph,
	);
}

/// Graph M retains every file owner, old pin and final cold path.
#[test]
fn graph_owner_sync_edge_mixed_ancestors_retains_all_owners() {
	replace_depth_with_parent(
		IoMode::Sync,
		AtomKind::Edge,
		false,
		[GraphKind::Graph, GraphKind::Metagraph, GraphKind::Graph],
		GraphKind::Graph,
	);
}

/// Graph M unanchored replacement leaves the complete source unpublished.
#[test]
fn graph_owner_sync_unanchored_replacement_publishes_nothing() {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		replace_depth_with_parent(
			IoMode::Sync,
			kind,
			true,
			[GraphKind::Graph; 3],
			GraphKind::Graph,
		);
	}
}

/// Graph M and Graph ancestors preserve the unchanged four-owner rejection
/// gates.
#[test]
fn graph_owner_sync_top_sources() {
	source_tests::bad_top_sources_with_parent(
		IoMode::Sync,
		[GraphKind::Graph; 3],
		GraphKind::Graph,
	);
}

/// Graph M and Graph ancestors preserve the unchanged four-owner rejection
/// gates.
#[test]
fn graph_owner_sync_resources() {
	resource_tests::reject_with_parent(IoMode::Sync, [GraphKind::Graph; 3], GraphKind::Graph);
}

/// Graph M and Graph ancestors preserve the unchanged four-owner rejection
/// gates.
#[test]
fn graph_owner_sync_recovery() {
	recovery_tests::failures_with_parent(IoMode::Sync, [GraphKind::Graph; 3], GraphKind::Graph);
}

/// Graph M retains every file owner, old pin and final cold path.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_node_metagraph_ancestors_retains_all_owners() {
	replace_depth_with_parent(
		IoMode::NativeAsync,
		AtomKind::Node,
		false,
		[GraphKind::Metagraph; 3],
		GraphKind::Graph,
	);
}

/// Graph M retains every file owner, old pin and final cold path.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_edge_metagraph_ancestors_retains_all_owners() {
	replace_depth_with_parent(
		IoMode::NativeAsync,
		AtomKind::Edge,
		false,
		[GraphKind::Metagraph; 3],
		GraphKind::Graph,
	);
}

/// Graph M retains every file owner, old pin and final cold path.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_node_graph_ancestors_retains_all_owners() {
	replace_depth_with_parent(
		IoMode::NativeAsync,
		AtomKind::Node,
		false,
		[GraphKind::Graph; 3],
		GraphKind::Graph,
	);
}

/// Graph M retains every file owner, old pin and final cold path.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_edge_graph_ancestors_retains_all_owners() {
	replace_depth_with_parent(
		IoMode::NativeAsync,
		AtomKind::Edge,
		false,
		[GraphKind::Graph; 3],
		GraphKind::Graph,
	);
}

/// Graph M retains every file owner, old pin and final cold path.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_node_mixed_ancestors_retains_all_owners() {
	replace_depth_with_parent(
		IoMode::NativeAsync,
		AtomKind::Node,
		false,
		[GraphKind::Graph, GraphKind::Metagraph, GraphKind::Graph],
		GraphKind::Graph,
	);
}

/// Graph M retains every file owner, old pin and final cold path.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_edge_mixed_ancestors_retains_all_owners() {
	replace_depth_with_parent(
		IoMode::NativeAsync,
		AtomKind::Edge,
		false,
		[GraphKind::Graph, GraphKind::Metagraph, GraphKind::Graph],
		GraphKind::Graph,
	);
}

/// Graph M unanchored replacement leaves the complete source unpublished.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_unanchored_replacement_publishes_nothing() {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		replace_depth_with_parent(
			IoMode::NativeAsync,
			kind,
			true,
			[GraphKind::Graph; 3],
			GraphKind::Graph,
		);
	}
}

/// Graph M and Graph ancestors preserve the unchanged four-owner rejection
/// gates.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_top_sources() {
	source_tests::bad_top_sources_with_parent(
		IoMode::NativeAsync,
		[GraphKind::Graph; 3],
		GraphKind::Graph,
	);
}

/// Graph M and Graph ancestors preserve the unchanged four-owner rejection
/// gates.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_resources() {
	resource_tests::reject_with_parent(
		IoMode::NativeAsync,
		[GraphKind::Graph; 3],
		GraphKind::Graph,
	);
}

/// Graph M and Graph ancestors preserve the unchanged four-owner rejection
/// gates.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_recovery() {
	recovery_tests::failures_with_parent(
		IoMode::NativeAsync,
		[GraphKind::Graph; 3],
		GraphKind::Graph,
	);
}

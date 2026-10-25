//! Genuine Graph owners retain child publication and accepted source bytes.

use super::*;

/// Graph replacement retains stable files through checkpoint and reopen.
#[test]
fn graph_owner_sync_node_replacement_retains_files_and_pins() {
	node_child_rebind_tests::rebind_with_family(IoMode::Sync, GraphKind::Graph);
}

/// Graph replacement retains stable files through checkpoint and reopen.
#[test]
fn graph_owner_sync_edge_replacement_retains_files_and_pins() {
	edge_child_rebind_tests::rebind_with_family(IoMode::Sync, GraphKind::Graph);
}

/// Graph child presence retains the selected source and old physical pin.
#[test]
fn graph_owner_sync_node_child_detach_retains_files_and_pins() {
	child_presence_tests::presence_with_family(
		IoMode::Sync,
		AtomKind::Node,
		false,
		GraphKind::Graph,
	);
}

/// Graph child presence retains the selected source and old physical pin.
#[test]
fn graph_owner_sync_node_child_attach_retains_files_and_pins() {
	child_presence_tests::presence_with_family(
		IoMode::Sync,
		AtomKind::Node,
		true,
		GraphKind::Graph,
	);
}

/// Graph child presence retains the selected source and old physical pin.
#[test]
fn graph_owner_sync_edge_child_detach_retains_files_and_pins() {
	child_presence_tests::presence_with_family(
		IoMode::Sync,
		AtomKind::Edge,
		false,
		GraphKind::Graph,
	);
}

/// Graph child presence retains the selected source and old physical pin.
#[test]
fn graph_owner_sync_edge_child_attach_retains_files_and_pins() {
	child_presence_tests::presence_with_family(
		IoMode::Sync,
		AtomKind::Edge,
		true,
		GraphKind::Graph,
	);
}

/// Graph retains local, qualified, wide and E-E role representations.
#[test]
fn graph_owner_sync_edge_role_matrix() {
	edge_child_rebind_matrix_tests::role_matrix_with_family(IoMode::Sync, GraphKind::Graph);
}

/// Graph NODE preparation and commit share the fixed logical budget.
#[test]
fn graph_owner_sync_node_resources() {
	node_child_rebind_resource_tests::copy_budget_with_family(IoMode::Sync, GraphKind::Graph);
}

/// Graph EDGE role growth rejects before any source publication.
#[test]
fn graph_owner_sync_edge_resources() {
	edge_child_rebind_resource_tests::role_budget_with_family(IoMode::Sync, GraphKind::Graph);
}

/// Graph NODE source faults preserve the publication boundary.
#[test]
fn graph_owner_sync_node_sources() {
	node_child_rebind_source_tests::bad_sources_with_family(IoMode::Sync, GraphKind::Graph);
}

/// Graph EDGE Hot and role-chain faults reject before publication.
#[test]
fn graph_owner_sync_edge_sources() {
	edge_child_rebind_source_tests::hot_and_authority_faults_with_family(
		IoMode::Sync,
		GraphKind::Graph,
	);
	edge_child_rebind_source_tests::chain_faults_with_family(IoMode::Sync, GraphKind::Graph);
}

/// Graph NODE failures recover one complete old or final image.
#[test]
fn graph_owner_sync_node_recovery() {
	node_child_rebind_recovery_tests::failures_with_family(IoMode::Sync, GraphKind::Graph);
}

/// Graph EDGE failures recover one complete old or final image.
#[test]
fn graph_owner_sync_edge_recovery() {
	edge_child_rebind_recovery_tests::failures_with_family(IoMode::Sync, GraphKind::Graph);
}

/// Graph replacement retains stable files through checkpoint and reopen.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_node_replacement_retains_files_and_pins() {
	node_child_rebind_tests::rebind_with_family(IoMode::NativeAsync, GraphKind::Graph);
}

/// Graph replacement retains stable files through checkpoint and reopen.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_edge_replacement_retains_files_and_pins() {
	edge_child_rebind_tests::rebind_with_family(IoMode::NativeAsync, GraphKind::Graph);
}

/// Graph child presence retains the selected source and old physical pin.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_node_child_detach_retains_files_and_pins() {
	child_presence_tests::presence_with_family(
		IoMode::NativeAsync,
		AtomKind::Node,
		false,
		GraphKind::Graph,
	);
}

/// Graph child presence retains the selected source and old physical pin.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_node_child_attach_retains_files_and_pins() {
	child_presence_tests::presence_with_family(
		IoMode::NativeAsync,
		AtomKind::Node,
		true,
		GraphKind::Graph,
	);
}

/// Graph child presence retains the selected source and old physical pin.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_edge_child_detach_retains_files_and_pins() {
	child_presence_tests::presence_with_family(
		IoMode::NativeAsync,
		AtomKind::Edge,
		false,
		GraphKind::Graph,
	);
}

/// Graph child presence retains the selected source and old physical pin.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_edge_child_attach_retains_files_and_pins() {
	child_presence_tests::presence_with_family(
		IoMode::NativeAsync,
		AtomKind::Edge,
		true,
		GraphKind::Graph,
	);
}

/// Graph retains local, qualified, wide and E-E role representations.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_edge_role_matrix() {
	edge_child_rebind_matrix_tests::role_matrix_with_family(IoMode::NativeAsync, GraphKind::Graph);
}

/// Graph NODE preparation and commit share the fixed logical budget.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_node_resources() {
	node_child_rebind_resource_tests::copy_budget_with_family(
		IoMode::NativeAsync,
		GraphKind::Graph,
	);
}

/// Graph EDGE role growth rejects before any source publication.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_edge_resources() {
	edge_child_rebind_resource_tests::role_budget_with_family(
		IoMode::NativeAsync,
		GraphKind::Graph,
	);
}

/// Graph NODE source faults preserve the publication boundary.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_node_sources() {
	node_child_rebind_source_tests::bad_sources_with_family(IoMode::NativeAsync, GraphKind::Graph);
}

/// Graph EDGE Hot and role-chain faults reject before publication.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_edge_sources() {
	edge_child_rebind_source_tests::hot_and_authority_faults_with_family(
		IoMode::NativeAsync,
		GraphKind::Graph,
	);
	edge_child_rebind_source_tests::chain_faults_with_family(IoMode::NativeAsync, GraphKind::Graph);
}

/// Graph NODE failures recover one complete old or final image.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_node_recovery() {
	node_child_rebind_recovery_tests::failures_with_family(IoMode::NativeAsync, GraphKind::Graph);
}

/// Graph EDGE failures recover one complete old or final image.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_edge_recovery() {
	edge_child_rebind_recovery_tests::failures_with_family(IoMode::NativeAsync, GraphKind::Graph);
}

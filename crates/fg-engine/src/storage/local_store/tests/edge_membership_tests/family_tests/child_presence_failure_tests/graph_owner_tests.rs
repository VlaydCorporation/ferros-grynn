//! Graph child presence and survivor remaps share atomic rejection gates.

use super::*;

/// Preserves the existing three-argument callback contract for Graph.
fn graph_new(mode: IoMode, kind: AtomKind, attach: bool) -> Case {
	Case::new_with_family(mode, kind, attach, GraphKind::Graph)
}

/// Preserves the existing three-argument callback contract for Graph.
fn graph_surviving(mode: IoMode, kind: AtomKind, attach: bool) -> Case {
	Case::surviving_with_family(mode, kind, attach, GraphKind::Graph)
}

/// Preserves the existing three-argument callback contract for Graph.
fn graph_qualified(mode: IoMode, kind: AtomKind, attach: bool) -> Case {
	Case::qualified_with_family(mode, kind, attach, GraphKind::Graph)
}

/// Preserves the existing three-argument callback contract for Graph.
fn graph_qualified_edge(mode: IoMode, kind: AtomKind, attach: bool) -> Case {
	Case::qualified_edge_with_family(mode, kind, attach, GraphKind::Graph)
}

/// Graph uses the unchanged resources assertions for new child scopes.
#[test]
fn graph_owner_sync_new_resources() {
	resources(IoMode::Sync, graph_new);
}

/// Graph uses the unchanged failures assertions for new child scopes.
#[test]
fn graph_owner_sync_new_failures() {
	failures(IoMode::Sync, graph_new);
}

/// Graph uses the unchanged replay assertions for new child scopes.
#[test]
fn graph_owner_sync_new_replay() {
	replay(IoMode::Sync, graph_new);
}

/// Graph uses the unchanged resources assertions for surviving child scopes.
#[test]
fn graph_owner_sync_surviving_resources() {
	resources(IoMode::Sync, graph_surviving);
}

/// Graph uses the unchanged failures assertions for surviving child scopes.
#[test]
fn graph_owner_sync_surviving_failures() {
	failures(IoMode::Sync, graph_surviving);
}

/// Graph uses the unchanged replay assertions for surviving child scopes.
#[test]
fn graph_owner_sync_surviving_replay() {
	replay(IoMode::Sync, graph_surviving);
}

/// Graph uses the unchanged resources assertions for qualified child scopes.
#[test]
fn graph_owner_sync_qualified_resources() {
	resources(IoMode::Sync, graph_qualified);
}

/// Graph uses the unchanged failures assertions for qualified child scopes.
#[test]
fn graph_owner_sync_qualified_failures() {
	failures(IoMode::Sync, graph_qualified);
}

/// Graph uses the unchanged replay assertions for qualified child scopes.
#[test]
fn graph_owner_sync_qualified_replay() {
	replay(IoMode::Sync, graph_qualified);
}

/// Graph uses the unchanged resources assertions for qualified_edge child
/// scopes.
#[test]
fn graph_owner_sync_qualified_edge_resources() {
	resources(IoMode::Sync, graph_qualified_edge);
}

/// Graph uses the unchanged failures assertions for qualified_edge child
/// scopes.
#[test]
fn graph_owner_sync_qualified_edge_failures() {
	failures(IoMode::Sync, graph_qualified_edge);
}

/// Graph uses the unchanged replay assertions for qualified_edge child scopes.
#[test]
fn graph_owner_sync_qualified_edge_replay() {
	replay(IoMode::Sync, graph_qualified_edge);
}

/// Graph uses the same literal source and private preparation oracles.
#[test]
fn graph_owner_sync_plain_sources() {
	source_tests::sources_with_family(IoMode::Sync, GraphKind::Graph);
}

/// Graph uses the same literal source and private preparation oracles.
#[test]
fn graph_owner_sync_survivor_sources() {
	survivor_source_tests::sources_with_family(IoMode::Sync, GraphKind::Graph);
}

/// Graph uses the same literal source and private preparation oracles.
#[test]
fn graph_owner_sync_qualified_node_sources() {
	qualified_source_tests::sources(IoMode::Sync, graph_qualified);
}

/// Graph uses the same literal source and private preparation oracles.
#[test]
fn graph_owner_sync_qualified_edge_sources() {
	qualified_source_tests::sources(IoMode::Sync, graph_qualified_edge);
}

/// Graph uses the same literal source and private preparation oracles.
#[test]
fn graph_owner_sync_endpoint_resources() {
	qualified_endpoint_tests::resources_with_family(IoMode::Sync, GraphKind::Graph);
}

/// Graph uses the same literal source and private preparation oracles.
#[test]
fn graph_owner_sync_endpoint_headers() {
	qualified_endpoint_tests::headers_with_family(IoMode::Sync, GraphKind::Graph);
}

/// Graph uses the same literal source and private preparation oracles.
#[test]
fn graph_owner_sync_extended_chain() {
	extended_chain_tests::cases_with_family(IoMode::Sync, GraphKind::Graph);
}

/// Graph uses the unchanged resources assertions for new child scopes.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_new_resources() {
	resources(IoMode::NativeAsync, graph_new);
}

/// Graph uses the unchanged failures assertions for new child scopes.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_new_failures() {
	failures(IoMode::NativeAsync, graph_new);
}

/// Graph uses the unchanged replay assertions for new child scopes.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_new_replay() {
	replay(IoMode::NativeAsync, graph_new);
}

/// Graph uses the unchanged resources assertions for surviving child scopes.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_surviving_resources() {
	resources(IoMode::NativeAsync, graph_surviving);
}

/// Graph uses the unchanged failures assertions for surviving child scopes.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_surviving_failures() {
	failures(IoMode::NativeAsync, graph_surviving);
}

/// Graph uses the unchanged replay assertions for surviving child scopes.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_surviving_replay() {
	replay(IoMode::NativeAsync, graph_surviving);
}

/// Graph uses the unchanged resources assertions for qualified child scopes.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_qualified_resources() {
	resources(IoMode::NativeAsync, graph_qualified);
}

/// Graph uses the unchanged failures assertions for qualified child scopes.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_qualified_failures() {
	failures(IoMode::NativeAsync, graph_qualified);
}

/// Graph uses the unchanged replay assertions for qualified child scopes.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_qualified_replay() {
	replay(IoMode::NativeAsync, graph_qualified);
}

/// Graph uses the unchanged resources assertions for qualified_edge child
/// scopes.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_qualified_edge_resources() {
	resources(IoMode::NativeAsync, graph_qualified_edge);
}

/// Graph uses the unchanged failures assertions for qualified_edge child
/// scopes.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_qualified_edge_failures() {
	failures(IoMode::NativeAsync, graph_qualified_edge);
}

/// Graph uses the unchanged replay assertions for qualified_edge child scopes.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_qualified_edge_replay() {
	replay(IoMode::NativeAsync, graph_qualified_edge);
}

/// Graph uses the same literal source and private preparation oracles.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_plain_sources() {
	source_tests::sources_with_family(IoMode::NativeAsync, GraphKind::Graph);
}

/// Graph uses the same literal source and private preparation oracles.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_survivor_sources() {
	survivor_source_tests::sources_with_family(IoMode::NativeAsync, GraphKind::Graph);
}

/// Graph uses the same literal source and private preparation oracles.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_qualified_node_sources() {
	qualified_source_tests::sources(IoMode::NativeAsync, graph_qualified);
}

/// Graph uses the same literal source and private preparation oracles.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_qualified_edge_sources() {
	qualified_source_tests::sources(IoMode::NativeAsync, graph_qualified_edge);
}

/// Graph uses the same literal source and private preparation oracles.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_endpoint_resources() {
	qualified_endpoint_tests::resources_with_family(IoMode::NativeAsync, GraphKind::Graph);
}

/// Graph uses the same literal source and private preparation oracles.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_endpoint_headers() {
	qualified_endpoint_tests::headers_with_family(IoMode::NativeAsync, GraphKind::Graph);
}

/// Graph uses the same literal source and private preparation oracles.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_owner_native_async_extended_chain() {
	extended_chain_tests::cases_with_family(IoMode::NativeAsync, GraphKind::Graph);
}

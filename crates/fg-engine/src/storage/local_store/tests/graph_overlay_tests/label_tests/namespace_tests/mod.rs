//! Final label ownership must remain encodable after ANALYZE and cold reopen.

use super::*;
use crate::datastore::{MaintenanceAnalyzeMode, MaintenanceTarget, QueryStatisticsDemand};

/// Stable namespace and overlay fields compose with plain membership DML.
mod membership_tests;

/// Sync retains stale distributions by name while discarding orphan owners.
#[test]
fn label_histogram_sync_namespace_changes_preserve_snapshots_and_reopen() {
	namespace_histograms(IoMode::Sync);
}

/// Actual NativeAsync uses the same final statistics ownership contract.
#[cfg(feature = "native-async-io")]
#[test]
fn label_histogram_native_async_namespace_changes_preserve_snapshots_and_reopen() {
	namespace_histograms(IoMode::NativeAsync);
}

/// Exercises leading insertion and last-member removal against a real analysis.
fn namespace_histograms(io_mode: IoMode) {
	let mut fixture = label_fixture(io_mode);
	let graph = fixture.graph.id();
	assert!(fixture.store.inner.current_root().indexes.has_deferred_indexes());
	fixture
		.store
		.run_foreground_maintenance_with_analysis(
			MaintenanceJobKind::Analyze,
			MaintenanceTarget::Graph(graph),
			fixture.options.maintenance,
			Some(MaintenanceAnalyzeMode::Full),
		)
		.assert_ok();
	fixture = reopen_checkpointed(fixture, io_mode);
	for (kind, target) in [
		(MaintenanceJobKind::CompactGraph, MaintenanceTarget::Graph(graph)),
		(MaintenanceJobKind::CompactDatabase, MaintenanceTarget::Database),
	] {
		assert!(fixture.store.inner.current_root().indexes.has_deferred_indexes());
		fixture
			.store
			.run_foreground_maintenance(kind, target, fixture.options.maintenance)
			.assert_ok();
		fixture = reopen_checkpointed(fixture, io_mode);
	}
	let before = fixture.store.inner.current_root();
	assert!(before.statistics.auxiliary_deferred());
	let oracle = crate::stats::analyze_database(&before).assert_ok();
	let source = oracle.graph(graph).assert_some();
	assert!(source.degree_histograms.keys().any(|key| key.label.is_some()));
	let pin = local_store(&fixture.store)
		.capture_query_snapshot(before.commit_ts)
		.assert_ok()
		.assert_some();
	let analyzed_revision = before.statistics.graph(graph).assert_some().analyzed_revision;
	let files = local_store(&fixture.store).state.lock().file_catalog.files.clone();
	let mut leading = fixture.store.begin();
	leading.set_occurrence_label(graph, &fixture.records[0], Some("A".into())).assert_ok();
	leading.commit().assert_ok();
	assert_namespace_unaffected_files(&fixture.store, graph, &files);
	let after = fixture.store.inner.current_root();
	let statistics = after.statistics.graph(graph).assert_some();
	assert_eq!(statistics.degree_histograms, source.degree_histograms);
	assert_eq!(statistics.analyzed_revision, analyzed_revision);
	assert_label_arrays(&fixture.store, graph);
	let mut remove = fixture.store.begin();
	remove.set_occurrence_label(graph, &fixture.records[2], None).assert_ok();
	remove.commit().assert_ok();
	assert_namespace_unaffected_files(&fixture.store, graph, &files);
	let expected = source
		.degree_histograms
		.iter()
		.filter(|(key, _)| key.label.as_ref().is_none_or(|label| label.label.as_str() != "M"))
		.map(|(key, histogram)| (key.clone(), histogram.clone()))
		.collect::<BTreeMap<_, _>>();
	assert!(expected.len() < source.degree_histograms.len());
	assert_eq!(
		fixture.store.inner.current_root().statistics.graph(graph).assert_some().degree_histograms,
		expected
	);
	assert_label_arrays(&fixture.store, graph);
	fixture.store.checkpoint(CheckpointMode::Passive).assert_ok();
	let demand = QueryStatisticsDemand {
		graph_labels: [graph].into_iter().collect(),
		graph_degrees: [graph].into_iter().collect(),
		..QueryStatisticsDemand::default()
	};
	let mut old = before.statistics.as_ref().clone();
	local_store(&fixture.store).load_planner_statistics(&pin, &before, &demand, &mut old);
	assert_eq!(old.graph(graph).assert_some().degree_histograms, source.degree_histograms);
	assert_eq!(old.graph(graph).assert_some().labels, source.labels);
	drop(pin);
	fixture = reopen_checkpointed(fixture, io_mode);
	let mut reopened = fixture.store.inner.current_root().as_ref().clone();
	fixture.store.inner.hydrate_statistics(&mut reopened).assert_ok();
	let statistics = reopened.statistics.graph(graph).assert_some();
	assert_eq!(statistics.degree_histograms, expected);
	assert_eq!(statistics.analyzed_revision, analyzed_revision);
	assert!(statistics.dirty_rows > 0);
	assert_label_arrays(&fixture.store, graph);
}

/// Node namespace changes leave topology, scalar indexes and Edge names intact.
fn assert_namespace_unaffected_files(
	store: &Datastore,
	graph: GraphId,
	before: &[FileCatalogEntry],
) {
	let local = local_store(store);
	let state = local.state.lock();
	for entry in before {
		let selected = entry.space_id == DataSpaceId::Graph(graph)
			&& (matches!(
				entry.file_id.kind(),
				Some(
					DataFileKind::Labels | DataFileKind::Statistics | DataFileKind::LabelStatistics
				)
			) || entry.file_id.kind() == Some(DataFileKind::Index)
				&& entry.path.as_str().contains("/idx/lbl_node_"));
		if !selected {
			assert!(
				state.file_catalog.files.contains(entry),
				"namespace publication replaced unrelated file: {:?}",
				entry.path
			);
		}
	}
}

/// Closes the writer before reopening and checks the actual selected transport.
fn reopen_checkpointed(mut fixture: LabelFixture, io_mode: IoMode) -> LabelFixture {
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(fixture.store);
	fixture.store = Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
	assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, io_mode);
	fixture.store.inner.maintenance.shutdown();
	fixture
}

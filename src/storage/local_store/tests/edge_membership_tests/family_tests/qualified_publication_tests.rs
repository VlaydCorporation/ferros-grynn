//! Real point-DML regressions precede any qualified publication eligibility
//! change.

use super::*;
use crate::{
	datastore::OccurrenceState,
	graph::{EdgeEndpoint, QualifiedAtomId},
};

/// Each shape exercises a different persisted role representation.
#[derive(Clone, Copy)]
enum Shape {
	/// Both roles fit inline with their explicit scopes.
	Inline,
	/// A repeated same-scope source must use whole-role overflow.
	Wide,
	/// Two source scopes include root zero and one nested terminal.
	Mixed,
	/// Target role exercises its own qualified chain and side tag.
	WideTarget,
	/// An undirected mixed role leaves the target role empty.
	Undirected,
}

/// Existing child bindings remain unchanged for each structural operation.
#[derive(Clone, Copy)]
enum Change {
	/// Attach an existing Base EDGE with a qualified NODE endpoint.
	Attach,
	/// Remove an accepted connector whose topology is already qualified.
	Detach,
	/// Replace a local role with an accepted qualified terminal.
	Rewire,
}

/// Sync ATTACH must not rebuild AtomStore or unrelated graph files.
#[test]
fn qualified_publication_sync_attach_retains_data_files() {
	qualified_publication(IoMode::Sync, Change::Attach, Shape::Inline);
}

/// NativeAsync ATTACH uses the declared backend after cold setup and recovery.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_publication_native_async_attach_retains_data_files() {
	qualified_publication(IoMode::NativeAsync, Change::Attach, Shape::Inline);
}

/// Sync DETACH validates and retires only the selected qualified connector.
#[test]
fn qualified_publication_sync_detach_retains_data_files() {
	qualified_publication(IoMode::Sync, Change::Detach, Shape::Inline);
}

/// NativeAsync DETACH preserves the accepted arena and genuine child files.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_publication_native_async_detach_retains_data_files() {
	qualified_publication(IoMode::NativeAsync, Change::Detach, Shape::Inline);
}

/// Sync local-to-qualified rewire keeps all graph data identities.
#[test]
fn qualified_publication_sync_rewire_retains_data_files() {
	qualified_publication(IoMode::Sync, Change::Rewire, Shape::Inline);
}

/// NativeAsync rewire is checked against an independent bulk image and reopen.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_publication_native_async_rewire_retains_data_files() {
	qualified_publication(IoMode::NativeAsync, Change::Rewire, Shape::Inline);
}

/// A real shared family fixture includes independent NODE and EDGE containers.
fn qualified_publication(mode: IoMode, change: Change, shape: Shape) -> Fixture {
	let (mut fixture, _) = family_fixture(mode, GraphKind::Metagraph);
	let topology = qualified_topology(&fixture, shape);
	let record = fixture.records[if matches!(change, Change::Rewire) {
		0
	} else {
		2
	}]
	.clone();
	if matches!(change, Change::Detach) {
		let mut setup = fixture.store.begin();
		setup
			.attach_edge(fixture.graph.id(), &record, None, topology.clone(), None, None)
			.assert_ok();
		setup.commit().assert_ok();
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = reopen(fixture);
	}
	assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
	assert_bulk_image(&fixture);
	let witness = metagraph::Witness::capture(&fixture);
	let files = local(&fixture.store).state.lock().file_catalog.files.clone();
	let mut write = fixture.store.begin();
	match change {
		Change::Attach => {
			write.attach_edge(fixture.graph.id(), &record, None, topology, None, None).assert_ok();
		}
		Change::Detach => {
			write.detach_occurrence(fixture.graph.id(), &record, false).assert_ok();
		}
		Change::Rewire => {
			write
				.replace_occurrence_state(
					fixture.graph.id(),
					&record,
					OccurrenceState::edge(topology, None, None),
				)
				.assert_ok();
		}
	}
	write.commit().assert_ok();
	// The existing retention oracle excludes only selected index/statistics
	// images.
	assert_retained(&fixture, &files, false);
	witness.assert_unchanged(&fixture);
	assert_bulk_image(&fixture);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
	let present =
		fixture.store.begin().membership(fixture.graph.id(), &record).assert_ok().is_some();
	assert_eq!(present, !matches!(change, Change::Detach));
	assert_retained(&fixture, &files, false);
	witness.assert_unchanged(&fixture);
	assert_bulk_image(&fixture);
	fixture
}

/// Resolves fixture identities through the SDK; the physical oracle uses only
/// page bytes.
fn qualified_topology(fixture: &Fixture, shape: Shape) -> EdgeTopology {
	let root = fixture.store.inner.current_root();
	let child = root.schema.named_graph("child_node").assert_some().reference().id();
	let mut read = fixture.store.begin();
	let owner = read
		.membership(fixture.graph.id(), &"node:container".parse().assert_ok())
		.assert_ok()
		.assert_some();
	let terminal =
		read.membership(child, &"node:child_node".parse().assert_ok()).assert_ok().assert_some();
	let nested = EdgeEndpoint::Nested(QualifiedAtomId::new([owner], terminal).assert_ok());
	let local = EdgeEndpoint::Local(fixture.nodes[0]);
	let target = EdgeEndpoint::Local(fixture.nodes[1]);
	match shape {
		Shape::Inline => EdgeTopology::directed([nested], [target]),
		Shape::Wide => EdgeTopology::directed([nested.clone(), nested.clone(), nested], [target]),
		Shape::Mixed => EdgeTopology::directed([local, nested], [target]),
		Shape::WideTarget => {
			EdgeTopology::directed([local], [nested.clone(), nested.clone(), nested])
		}
		Shape::Undirected => EdgeTopology::undirected([local.clone(), nested, local]),
	}
}

/// Sync wide/mixed roles preserve scopes, duplicate order and empty undirected
/// target.
#[test]
fn qualified_publication_sync_overflow_matrix_matches_bulk() {
	overflow_matrix(IoMode::Sync);
}

/// NativeAsync applies the same ATTACH/DETACH/rewire matrix and cold oracle.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_publication_native_async_overflow_matrix_matches_bulk() {
	overflow_matrix(IoMode::NativeAsync);
}

/// Every declared cell uses a fresh source and performs a real production
/// commit.
fn overflow_matrix(mode: IoMode) {
	for shape in [Shape::Wide, Shape::Mixed, Shape::WideTarget, Shape::Undirected] {
		for change in [Change::Attach, Change::Detach, Change::Rewire] {
			qualified_publication(mode, change, shape);
		}
	}
}

/// Sync repeated rewrites append beyond the old prefix and survive
/// backfill/reopen.
#[test]
fn qualified_publication_sync_overflow_churn_matches_bulk() {
	overflow_churn(IoMode::Sync);
}

/// NativeAsync preserves the same data files across both overflow side tags.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_publication_native_async_overflow_churn_matches_bulk() {
	overflow_churn(IoMode::NativeAsync);
}

/// The full bulk image is rebuilt independently after every representation
/// change.
fn overflow_churn(mode: IoMode) {
	let mut fixture = qualified_publication(mode, Change::Attach, Shape::Wide);
	let files = local(&fixture.store).state.lock().file_catalog.files.clone();
	let witness = metagraph::Witness::capture(&fixture);
	for shape in [
		Shape::Inline,
		Shape::Mixed,
		Shape::Wide,
		Shape::WideTarget,
		Shape::Undirected,
		Shape::Inline,
		Shape::Wide,
	] {
		let topology = qualified_topology(&fixture, shape);
		let mut write = fixture.store.begin();
		write
			.replace_occurrence_state(
				fixture.graph.id(),
				&fixture.records[2],
				OccurrenceState::edge(topology, None, None),
			)
			.assert_ok();
		write.commit().assert_ok();
		assert_retained(&fixture, &files, false);
		witness.assert_unchanged(&fixture);
		assert_bulk_image(&fixture);
		fixture.store.checkpoint(CheckpointMode::Passive).assert_ok();
		assert_bulk_image(&fixture);
	}
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
	assert_retained(&fixture, &files, false);
	witness.assert_unchanged(&fixture);
	assert_bulk_image(&fixture);
}

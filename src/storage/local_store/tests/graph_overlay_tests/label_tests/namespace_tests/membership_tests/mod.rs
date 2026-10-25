//! Plain lifecycle and stable metadata must share one final physical image.

use super::*;
use crate::datastore::Transaction;

/// Cross-page and cross-graph publication, snapshots and replay.
mod boundary_tests;
/// Full local/global generations across labelled create and reuse.
mod label_lifecycle_reuse_tests;
/// Explicit same-owner ownership and namespace suffix differential.
mod label_lifecycle_synthetic_tests;
/// Labelled lifecycle shares sparse identities and final dictionary ceilings.
mod label_lifecycle_tests;
/// Independent decoded Hot, field, index and checkpoint assertions.
mod oracles;
/// Shared Hot/source codec peak uses the confirmed admission boundary.
mod overlay_lifecycle_resource_tests;
/// Full local/global generations and transient payload ownership.
mod overlay_lifecycle_reuse_tests;
/// Physical source trust boundaries and prepared page uniqueness.
mod overlay_lifecycle_source_tests;
/// Lifecycle payloads share one sparse graph image with stable metadata.
mod overlay_lifecycle_tests;
/// Known rejection, uncertain WAL completion and bounded admission.
mod recovery_tests;
/// Payload conversion and page backing share the physical publication budget.
mod resource_tests;

/// A stable endpoint patch cannot turn an ordinary NODE attach into a rebuild.
#[test]
fn metadata_membership_node_sync_preserves_files_and_reopens() {
	mixed_membership(IoMode::Sync, AtomKind::Node);
}

/// Native NODE publication keeps the selected transport across cold recovery.
#[cfg(feature = "native-async-io")]
#[test]
fn metadata_membership_node_native_async_preserves_files_and_reopens() {
	mixed_membership(IoMode::NativeAsync, AtomKind::Node);
}

/// EDGE adjacency and stable endpoint fields can share the same NODE Hot page.
#[test]
fn metadata_membership_edge_sync_preserves_files_and_reopens() {
	mixed_membership(IoMode::Sync, AtomKind::Edge);
}

/// Native EDGE publication preserves the same combined metadata authority.
#[cfg(feature = "native-async-io")]
#[test]
fn metadata_membership_edge_native_async_preserves_files_and_reopens() {
	mixed_membership(IoMode::NativeAsync, AtomKind::Edge);
}

/// Existing and new Base owners exercise the same final replay composition.
fn mixed_membership(io_mode: IoMode, kind: AtomKind) {
	for new_owner in [false, true] {
		let (mut fixture, pending) = membership_fixture(io_mode);
		let graph = fixture.graph.id();
		let local = local_store(&fixture.store);
		let files = local.state.lock().file_catalog.files.clone();
		let boundary = local.wal.snapshot().visible_lsn();
		let record = if new_owner {
			match kind {
				AtomKind::Node => "item:created",
				AtomKind::Edge => "link:created",
			}
			.parse::<RecordId>()
			.assert_ok()
		} else {
			match kind {
				AtomKind::Node => pending[0].clone(),
				AtomKind::Edge => pending[1].clone(),
			}
		};
		membership_candidate(&fixture, &record, kind, new_owner).commit().assert_ok();
		super::super::namespace_publication_tests::assert_namespace_files(&fixture, &files);
		let frames = local
			.wal
			.recovery_frames()
			.into_iter()
			.filter(|frame| frame.frame_lsn() > boundary)
			.collect::<Vec<_>>();
		let keys =
			frames.iter().map(|frame| frame.key()).collect::<std::collections::BTreeSet<_>>();
		assert_eq!(keys.len(), frames.len());
		assert!(!keys.is_empty());
		assert_label_arrays(&fixture.store, graph);
		assert_overlay_property(
			&fixture.store,
			graph,
			&fixture.records[0],
			fixture.keys[0],
			10_000.into(),
		);
		assert!(fixture.store.begin().membership(graph, &record).assert_ok().is_some());
		oracles::assert_image(&fixture);
		fixture = reopen_checkpointed(fixture, io_mode);
		assert_label_arrays(&fixture.store, graph);
		assert_overlay_property(
			&fixture.store,
			graph,
			&fixture.records[0],
			fixture.keys[0],
			10_000.into(),
		);
		assert!(fixture.store.begin().membership(graph, &record).assert_ok().is_some());
		oracles::assert_image(&fixture);
	}
}

/// Base, stable NODE fields and stable EDGE weight/label share one candidate.
fn membership_candidate(
	fixture: &LabelFixture,
	record: &RecordId,
	kind: AtomKind,
	new_owner: bool,
) -> Transaction {
	let graph = fixture.graph.id();
	let mut write = fixture.store.begin();
	if new_owner {
		write.create_atom(record.clone(), membership_properties(fixture, kind)).assert_ok();
	}
	match kind {
		AtomKind::Node => {
			write.attach_node(graph, record, None, None).assert_ok();
		}
		AtomKind::Edge => {
			write
				.attach_edge(
					graph,
					record,
					None,
					EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]]),
					None,
					None,
				)
				.assert_ok();
		}
	}
	write.set_overlay(graph, &fixture.records[0], fixture.keys[0], 10_000.into()).assert_ok();
	// The overlay hides the intermediate Unique key already owned by padding.
	write.set_base_property(&fixture.records[0], fixture.keys[0], 17.into()).assert_ok();
	write.set_occurrence_label(graph, &fixture.records[0], Some("A".into())).assert_ok();
	write.set_occurrence_label(graph, &fixture.records[3], Some("A".into())).assert_ok();
	write
		.replace_occurrence_state(
			graph,
			&fixture.records[3],
			OccurrenceState::edge(
				EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]]),
				Some(3_i64.into()),
				None,
			),
		)
		.assert_ok();
	write
}

/// The label fixture already owns M/N/Z names and independent scalar oracles.
fn membership_fixture(io_mode: IoMode) -> (LabelFixture, [RecordId; 2]) {
	let mut fixture = label_fixture(io_mode);
	let pending = ["item:pending", "link:pending"].map(|text| text.parse::<RecordId>().assert_ok());
	let mut seed = fixture.store.begin();
	seed.define_graph("other", GraphKind::Graph).assert_ok();
	seed.define_index(
		"item",
		ClassIndex::new("base", ClassIndexKind::Standard, IndexScope::Base, ["value"]).assert_ok(),
	)
	.assert_ok();
	seed.define_index(
		"item",
		ClassIndex::new(
			"members",
			ClassIndexKind::Count,
			IndexScope::Effective {
				graph: fixture.graph.id(),
			},
			None::<&str>,
		)
		.assert_ok(),
	)
	.assert_ok();
	for (record, kind) in pending.iter().zip([AtomKind::Node, AtomKind::Edge]) {
		seed.create_atom(record.clone(), membership_properties(&fixture, kind)).assert_ok();
	}
	seed.commit().assert_ok();
	fixture = reopen_checkpointed(fixture, io_mode);
	// The next append must follow the WAL-visible tail, rather than home
	// length.
	let mut source = fixture.store.begin();
	source.hide_property(fixture.graph.id(), &fixture.records[0], fixture.keys[1]).assert_ok();
	source.commit().assert_ok();
	(fixture, pending)
}

/// Strict item fields and plain link ownership retain the existing schema.
fn membership_properties(fixture: &LabelFixture, kind: AtomKind) -> PropertyMap {
	let mut properties = PropertyMap::new();
	if kind == AtomKind::Node {
		properties.insert(fixture.keys[0], 1000_i64);
		properties.insert(fixture.keys[1], "base");
	}
	properties
}

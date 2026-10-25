//! Bulk Hot records and physical index probes are independent durable oracles.

use super::*;
use crate::{
	storage::{
		graph_pages::{build_graph_image, hot_slot_address, read_node_hot_slot},
		page::{PageLsn, PageNo},
		types::{Lsn, PageKey},
	},
	values::storage::EncodeContext,
};

/// Compares Hot slots with bulk and probes every scalar scope, including the
/// untouched Base-only control row.
pub(super) fn assert_image(fixture: &Fixture, expected: &[(RecordId, i64)]) {
	let root = fixture.store.inner.current_root();
	let graph = fixture.graph.id();
	let local = local_store(&fixture.store);
	let mut context = EncodeContext::new(root.dictionary.as_ref().clone());
	let bulk = build_graph_image(&root, graph, &mut context, PageLsn::ZERO).assert_ok();
	{
		let state = local.state.lock();
		let files = state
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == graph)
			.assert_some()
			.files;
		let header = state.graph_superblocks[&graph];
		assert_eq!(
			(header.node_count, header.node_capacity),
			(u64::try_from(expected.len()).assert_ok(), bulk.superblock.node_capacity)
		);
		for slot in 0..header.node_capacity {
			let (number, within) = hot_slot_address(slot).assert_ok();
			let page = local
				.read_update_page(&state, PageKey::new(files.space(), files.nodes, number))
				.assert_ok();
			assert_eq!(
				read_node_hot_slot(&page, number, within).assert_ok(),
				read_node_hot_slot(
					&bulk.nodes[usize::try_from(number.get()).assert_ok()],
					number,
					within
				)
				.assert_ok()
			);
		}
	}
	let physical = local.capture_query_snapshot(root.commit_ts).assert_ok().assert_some();
	let class = root.schema.class("item").assert_some();
	let mut read = fixture.store.begin();
	let snapshot = read.query_snapshot().assert_ok();
	let anchor = "item:untouched".parse::<RecordId>().assert_ok();
	let has_anchor = root.resolve_atom(&anchor).is_some();
	let mut checked = 0;
	for index in class.indexes() {
		checked += 1;
		let scope = match index.scope() {
			IndexScope::Base => None,
			IndexScope::Effective {
				graph: owner,
			} => {
				assert_eq!(owner, graph);
				Some(&fixture.graph)
			}
		};
		let count = if scope.is_none() {
			root.atoms.atoms().filter(|atom| Some(atom.class_id()) == class.id()).count()
		} else {
			expected.len()
		};
		let entries =
			local.physical_index(&physical, index.id().assert_some()).assert_ok().assert_some();
		match &entries.entries {
			PhysicalIndexEntries::Count(actual) => {
				assert_eq!(*actual, u64::try_from(count).assert_ok());
			}
			PhysicalIndexEntries::Scalar(groups) => {
				assert_eq!(groups.iter().map(|group| group.atoms.len()).sum::<usize>(), count);
				for (record, value) in expected
					.iter()
					.map(|(record, value)| (record, *value))
					.chain((scope.is_none() && has_anchor).then_some((&anchor, -1)))
				{
					let mut cursor = read
						.query_index_probe_cursor(
							&snapshot,
							"item",
							scope,
							index.id().assert_some(),
							&[Value::from(value)],
						)
						.assert_ok();
					assert_eq!(cursor.next_row().assert_ok().assert_some().record_id, *record);
					assert!(cursor.next_row().assert_ok().is_none());
				}
			}
			_ => unreachable!("scalar fixture changed index kind"),
		}
	}
	assert_eq!(checked, 4);
	for (record, value) in expected {
		let global = root.resolve_atom(record).assert_some();
		assert_eq!(
			root.atom(global).assert_some().properties().get(fixture.key),
			Some(&Value::from(*value))
		);
		assert!(root.membership(graph, global).is_some());
	}
}

/// Exact Hot sets and checkpoint tuples are independent of scalar fallback.
pub(super) fn assert_hot_frames(
	fixture: &Fixture,
	boundary: Lsn,
	global_pages: &[u64],
	node_pages: &[u64],
) {
	let local = local_store(&fixture.store);
	let frames = local
		.wal
		.recovery_frames()
		.into_iter()
		.filter(|frame| frame.frame_lsn() > boundary)
		.collect::<Vec<_>>();
	let keys = frames.iter().map(|frame| frame.key()).collect::<std::collections::BTreeSet<_>>();
	assert_eq!(keys.len(), frames.len(), "one commit must stage each key once");
	assert!(!frames.is_empty());
	assert!(keys.iter().all(|key| key.space_id() == DataSpaceId::Database
		|| key.space_id() == DataSpaceId::Graph(fixture.graph.id())));
	let state = local.state.lock();
	let atoms = state.bindings.atoms;
	let graph = state
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == fixture.graph.id())
		.assert_some()
		.files;
	for (kind, space, file, pages) in [
		(DataFileKind::AtomsHot, DataSpaceId::Database, atoms.atoms_hot, global_pages),
		(DataFileKind::NodesHot, graph.space(), graph.nodes, node_pages),
	] {
		let actual = keys
			.iter()
			.filter(|key| key.file_id().kind() == Some(kind))
			.copied()
			.collect::<std::collections::BTreeSet<_>>();
		let expected =
			pages.iter().map(|number| PageKey::new(space, file, PageNo::new(*number))).collect();
		assert_eq!(actual, expected);
	}
	let header = state.graph_superblocks[&fixture.graph.id()];
	for (generation, backfilled, record) in [
		(header.checkpoint_generation, header.backfilled_lsn, header.checkpoint_record_lsn),
		(
			state.atom_superblock.checkpoint_generation,
			state.atom_superblock.backfilled_lsn,
			state.atom_superblock.checkpoint_record_lsn,
		),
	] {
		assert_eq!(generation, state.database_superblock.generation);
		assert_eq!(backfilled, state.database_superblock.checkpoint_lsn);
		assert_eq!(record, state.database_superblock.checkpoint_record_lsn);
	}
}

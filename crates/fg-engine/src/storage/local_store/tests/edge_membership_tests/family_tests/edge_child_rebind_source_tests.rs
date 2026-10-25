//! Validly encoded EDGE source faults must never turn into a bulk fallback.

use std::sync::Arc;

use super::{
	edge_child_rebind_matrix_tests::shape_fixture_with_family,
	edge_child_rebind_support::{Source, edge_state},
	metagraph::{leaf_roots, prepare_leaf},
	*,
};
use crate::storage::{
	graph_pages::{
		EndpointSide,
		GraphArenaBindings,
		decode_cross_level_overflow,
		decode_subgraph_directory,
		encode_adjacency_overflow,
		encode_cross_level_overflow,
		encode_subgraph_directory,
		write_edge_hot_slot,
	},
	local_store::{LocalState, LocalStore},
	page::{AlignedPage, PageLsn},
};

/// Sync rejects complete Hot, canonical-owner, directory and role-chain faults.
#[test]
fn edge_child_rebind_sync_malformed_sources_preserve_publication_boundary() {
	hot_and_authority_faults(IoMode::Sync);
	chain_faults(IoMode::Sync);
}

/// NativeAsync restores the same passing preparation after every injected
/// fault.
#[cfg(feature = "native-async-io")]
#[test]
fn edge_child_rebind_native_async_malformed_sources_preserve_publication_boundary() {
	hot_and_authority_faults(IoMode::NativeAsync);
	chain_faults(IoMode::NativeAsync);
}

/// Deliberate semantic faults have a valid page codec and checksum.
fn stage(store: &LocalStore, key: PageKey, page: &AlignedPage) {
	let mut write = store.wal.begin_tx().assert_ok();
	write.stage_page(key, page).assert_ok();
	store.wal.commit_tx(write).assert_ok();
}

/// Both runtime authorities must retain their injected owners on rejection.
fn owners(
	state: &LocalState,
	graph: crate::graph::GraphId,
) -> (GraphArenaBindings, GraphArenaBindings) {
	(
		state
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == graph)
			.assert_some()
			.arena
			.clone(),
		state
			._database_catalog
			.graphs
			.iter()
			.find(|entry| entry.id == graph)
			.assert_some()
			.arena
			.clone(),
	)
}

/// Direct preparation isolates source policy from the intentionally injected
/// WAL clock.
fn reject(
	fixture: &Fixture,
	roots: (&crate::datastore::DatabaseRoot, &crate::datastore::DatabaseRoot),
	delta: &crate::datastore::PublicationDelta,
	expected_reason: Option<&str>,
) {
	let root = fixture.store.inner.current_root();
	let store = local(&fixture.store);
	let state = store.state.lock();
	let injected = owners(&state, fixture.graph.id());
	let files = state.file_catalog.files.clone();
	let boundary = store.wal.snapshot().visible_lsn();
	let directories = super::super::super::generation_directory_paths(&fixture.directory.0);
	drop(state);
	let error = prepare_leaf(store, roots, delta).err().assert_some();
	assert!(
		matches!(
			error.status().status(),
			crate::gql_status::Status::InvalidStorageFile { .. }
				| crate::gql_status::Status::InvalidStoragePage { .. }
		),
		"{error}"
	);
	if let Some(expected) = expected_reason {
		assert!(
			matches!(error.status().status(), crate::gql_status::Status::InvalidStorageFile { reason }
            if reason.as_str() == expected),
			"{error}"
		);
	}
	assert!(Arc::ptr_eq(&root, &fixture.store.inner.current_root()));
	assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
	let state = store.state.lock();
	let current = owners(&state, fixture.graph.id());
	assert!(current.0.same_owner(&injected.0));
	assert!(current.1.same_owner(&injected.1));
	assert_eq!(state.file_catalog.files, files);
	assert_eq!(super::super::super::generation_directory_paths(&fixture.directory.0), directories);
}

/// The EDGE source remains codec-valid, including wrong kind-correct global
/// IDs.
fn hot_fault(fixture: &Fixture, source: &Source, fault: usize) -> (PageKey, Box<AlignedPage>) {
	let store = local(&fixture.store);
	let state = store.state.lock();
	let (number, within) = hot_slot_address(source.owner.1.slot()).assert_ok();
	let files = source.graph_files;
	let key = PageKey::new(files.space(), files.edges, number);
	let original = store.read_update_page(&state, key).assert_ok();
	let mut page = store.read_update_page(&state, key).assert_ok();
	let mut hot = read_edge_hot_slot(&page, number, within).assert_ok();
	match fault {
		0 => {
			hot.generation += 1;
		}
		1 => {
			let other =
				fixture.store.inner.current_root().resolve_atom(&fixture.records[0]).assert_some();
			assert_eq!(other.kind(), Some(AtomKind::Edge));
			assert_ne!(other, source.owner.0);
			hot.global_atom = Some(other);
		}
		2 => {
			hot.transition_subgraph_slot = Some(
				*source
					.arena
					.graph_by_slot()
					.keys()
					.find(|slot| **slot != 0 && **slot != source.slot)
					.assert_some(),
			);
		}
		3 => {
			hot.replace_label(number, Some(0), Some(123)).assert_ok();
		}
		4 => {
			hot.replace_weight(number, Some(11.0), Some(9.0)).assert_ok();
		}
		5 => {
			hot.source_inline[0] =
				Some(DiskAtomRef::from_parts(AtomKind::Edge, source.owner.1.slot()).assert_ok());
		}
		_ => {
			assert_eq!(fault, 6);
			hot.target_inline[0] =
				Some(DiskAtomRef::from_parts(AtomKind::Node, fixture.nodes[0].slot()).assert_ok());
		}
	}
	write_edge_hot_slot(&mut page, within, &hot).assert_ok();
	page.seal().assert_ok();
	stage(store, key, &page);
	assert_eq!(
		read_edge_hot_slot(&store.read_update_page(&state, key).assert_ok(), number, within)
			.assert_ok(),
		hot
	);
	(key, original)
}

/// Fifteen isolated faults exercise each source authority before map copying.
fn hot_and_authority_faults(mode: IoMode) {
	hot_and_authority_faults_with_family(mode, GraphKind::Metagraph);
}

/// Runs the same fixture contract for the selected public parent family.
pub(super) fn hot_and_authority_faults_with_family(mode: IoMode, family: GraphKind) {
	for fault in 0..15 {
		let (fixture, key, replacement, _) = shape_fixture_with_family(mode, 4, family);
		let source = Source::capture(&fixture);
		let (mut before, mut after, delta) =
			leaf_roots(&fixture, source.owner.1, edge_state(&fixture, replacement));
		let store = local(&fixture.store);
		prepare_leaf(store, (&before, &after), &delta).assert_ok();
		if fault == 14 {
			// Source metadata is well-shaped but disagrees with its old bytes.
			before.set_overlay(fixture.graph.id(), source.owner.1, key, 888_i64.into()).assert_ok();
			after = before.clone();
			after
				.replace_occurrence_state(
					fixture.graph.id(),
					source.owner.1,
					edge_state(&fixture, replacement),
				)
				.assert_ok();
		}
		let state = store.state.lock();
		let old_header = *state.graph_superblocks.get(&fixture.graph.id()).assert_some();
		let replacement_header = *state.graph_superblocks.get(&replacement).assert_some();
		drop(state);
		let mut original = None;
		if fault <= 6 {
			original = Some(hot_fault(&fixture, &source, fault));
		} else {
			let mut state = store.state.lock();
			match fault {
				7 => {
					state
						.graph_superblocks
						.get_mut(&fixture.graph.id())
						.assert_some()
						.edge_capacity = source.owner.1.slot();
				}
				8 => {
					state
						.graph_superblocks
						.get_mut(&fixture.graph.id())
						.assert_some()
						.edge_hot_pages =
						hot_slot_address(source.owner.1.slot()).assert_ok().0.get();
				}
				9 | 10 => {
					let mut scopes = source.arena.scope_by_slot().clone();
					if fault == 10 {
						scopes.insert(
							source.slot,
							vec![
								AtomId::from_parts(
									AtomKind::Edge,
									source.owner.1.slot(),
									source.owner.1.generation() + 1,
								)
								.assert_ok(),
							]
							.into_boxed_slice(),
						);
					}
					let forged =
						GraphArenaBindings::new(source.arena.graph_by_slot().clone(), scopes);
					state
						.bindings
						.graphs
						.iter_mut()
						.find(|binding| binding.graph_id == fixture.graph.id())
						.assert_some()
						.arena = forged.clone();
					if fault == 10 {
						state
							._database_catalog
							.graphs
							.iter_mut()
							.find(|entry| entry.id == fixture.graph.id())
							.assert_some()
							.arena = forged;
					}
				}
				11 | 12 => {
					let files = source.graph_files;
					let key = PageKey::new(files.space(), files.subgraphs, PageNo::new(0));
					let page = store.read_update_page(&state, key).assert_ok();
					let (next, mut entries) =
						decode_subgraph_directory(&page, PageNo::new(0)).assert_ok();
					entries
						.iter_mut()
						.find(|entry| (entry.slot == source.slot) == (fault == 11))
						.assert_some()
						.node_count += 1;
					let forged =
						encode_subgraph_directory(PageNo::new(0), PageLsn::ZERO, next, &entries)
							.assert_ok();
					stage(store, key, &forged);
					assert_eq!(
						decode_subgraph_directory(
							&store.read_update_page(&state, key).assert_ok(),
							PageNo::new(0)
						)
						.assert_ok(),
						(next, entries)
					);
					original = Some((key, page));
				}
				13 => {
					state.graph_superblocks.get_mut(&replacement).assert_some().node_count += 1;
				}
				_ => {
					assert_eq!(fault, 14);
				}
			}
		}
		reject(&fixture, (&before, &after), &delta, None);
		let mut state = store.state.lock();
		state
			.bindings
			.graphs
			.iter_mut()
			.find(|binding| binding.graph_id == fixture.graph.id())
			.assert_some()
			.arena = source.arena.clone();
		state
			._database_catalog
			.graphs
			.iter_mut()
			.find(|entry| entry.id == fixture.graph.id())
			.assert_some()
			.arena = source.arena.clone();
		state.graph_superblocks.insert(fixture.graph.id(), old_header);
		state.graph_superblocks.insert(replacement, replacement_header);
		if let Some((key, page)) = original {
			stage(store, key, &page);
		}
		drop(state);
		let (before, after, delta) =
			leaf_roots(&fixture, source.owner.1, edge_state(&fixture, replacement));
		prepare_leaf(store, (&before, &after), &delta).assert_ok();
	}
}

/// Ordinary and qualified chains preserve old bounds, order, roles and counts.
fn chain_faults(mode: IoMode) {
	chain_faults_with_family(mode, GraphKind::Metagraph);
}

/// Runs the same fixture contract for the selected public parent family.
pub(super) fn chain_faults_with_family(mode: IoMode, family: GraphKind) {
	for qualified in [false, true] {
		let faults = if qualified {
			8
		} else {
			5
		};
		for fault in 0..faults {
			let (fixture, _, replacement, _) =
				shape_fixture_with_family(mode, usize::from(qualified), family);
			let source = Source::capture(&fixture);
			let (before, after, delta) =
				leaf_roots(&fixture, source.owner.1, edge_state(&fixture, replacement));
			let store = local(&fixture.store);
			prepare_leaf(store, (&before, &after), &delta).assert_ok();
			let (key, original) = if qualified {
				qualified_fault(&fixture, &source, fault)
			} else {
				ordinary_fault(&fixture, &source, fault)
			};
			let reason = (fault == 1).then_some(if qualified {
				"qualified EDGE chain exceeds its source ceiling/count or contains a cycle"
			} else {
				"adjacency source exceeds its ceiling or contains a cycle"
			});
			reject(&fixture, (&before, &after), &delta, reason);
			stage(store, key, &original);
			prepare_leaf(store, (&before, &after), &delta).assert_ok();
		}
	}
}

/// An available valid WAL tail still lies outside the accepted ordinary
/// ceiling.
fn ordinary_fault(fixture: &Fixture, source: &Source, fault: usize) -> (PageKey, Box<AlignedPage>) {
	let store = local(&fixture.store);
	let state = store.state.lock();
	let files = source.graph_files;
	let ceiling =
		state.graph_superblocks.get(&fixture.graph.id()).assert_some().adjacency_overflow_pages;
	let number = source.hot.source_overflow.assert_some();
	let key = PageKey::new(files.space(), files.adjacency, number);
	let original = store.read_update_page(&state, key).assert_ok();
	let (old_next, mut entries) = decode_adjacency_overflow(&original, number).assert_ok();
	assert!(old_next.is_some());
	assert_ne!(entries[0], entries[1]);
	let next = match fault {
		0 => Some(number),
		1 => Some(PageNo::new(ceiling)),
		2 => {
			entries.truncate(1);
			None
		}
		3 => {
			entries[0] = DiskAtomRef::from_parts(AtomKind::Edge, source.owner.1.slot()).assert_ok();
			old_next
		}
		_ => {
			assert_eq!(fault, 4);
			entries.swap(0, 1);
			old_next
		}
	};
	let forged = encode_adjacency_overflow(number, PageLsn::ZERO, next, &entries).assert_ok();
	stage(store, key, &forged);
	assert_eq!(
		decode_adjacency_overflow(&store.read_update_page(&state, key).assert_ok(), number)
			.assert_ok(),
		(next, entries.clone())
	);
	if fault == 1 {
		let tail_key = PageKey::new(files.space(), files.adjacency, PageNo::new(ceiling));
		// Reuse the real remaining suffix, so removing only the old
		// ceiling guard would otherwise accept this complete ordered role.
		let suffix_number = old_next.assert_some();
		let suffix_key = PageKey::new(files.space(), files.adjacency, suffix_number);
		let (suffix_next, suffix) = decode_adjacency_overflow(
			&store.read_update_page(&state, suffix_key).assert_ok(),
			suffix_number,
		)
		.assert_ok();
		assert_eq!(suffix_next, None);
		assert_eq!(
			2 + entries.len() + suffix.len(),
			usize::try_from(source.hot.source_count).assert_ok()
		);
		let tail =
			encode_adjacency_overflow(tail_key.page_no(), PageLsn::ZERO, suffix_next, &suffix)
				.assert_ok();
		stage(store, tail_key, &tail);
		assert_eq!(
			decode_adjacency_overflow(
				&store.read_update_page(&state, tail_key).assert_ok(),
				tail_key.page_no()
			)
			.assert_ok(),
			(suffix_next, suffix)
		);
	}
	(key, original)
}

/// Qualified overflow stores the whole role and requires exact
/// scope/side/order.
fn qualified_fault(
	fixture: &Fixture,
	source: &Source,
	fault: usize,
) -> (PageKey, Box<AlignedPage>) {
	let store = local(&fixture.store);
	let state = store.state.lock();
	let files = source.graph_files;
	let ceiling = state
		.graph_superblocks
		.get(&fixture.graph.id())
		.assert_some()
		.cross_level_adjacency_overflow_pages;
	let number = source.hot.source_subgraph_overflow.assert_some();
	let key = PageKey::new(files.space(), files.cross_level_adjacency, number);
	let original = store.read_update_page(&state, key).assert_ok();
	let (old_next, mut entries) = decode_cross_level_overflow(&original, number).assert_ok();
	assert!(old_next.is_some());
	assert_ne!(entries[0].graph_slot, entries[1].graph_slot);
	let next = match fault {
		0 => Some(number),
		1 => Some(PageNo::new(ceiling)),
		2 => {
			entries.truncate(1);
			None
		}
		3 => {
			entries[0].side = EndpointSide::Target;
			old_next
		}
		4 => {
			entries[0].graph_slot = u32::MAX;
			old_next
		}
		5 => {
			entries[0].atom =
				DiskAtomRef::from_parts(AtomKind::Edge, source.owner.1.slot()).assert_ok();
			old_next
		}
		6 => {
			entries.swap(0, 1);
			old_next
		}
		_ => {
			assert_eq!(fault, 7);
			entries.clear();
			old_next
		}
	};
	let forged = encode_cross_level_overflow(number, PageLsn::ZERO, next, &entries).assert_ok();
	stage(store, key, &forged);
	assert_eq!(
		decode_cross_level_overflow(&store.read_update_page(&state, key).assert_ok(), number)
			.assert_ok(),
		(next, entries.clone())
	);
	if fault == 1 {
		let tail_key =
			PageKey::new(files.space(), files.cross_level_adjacency, PageNo::new(ceiling));
		// Keep the accepted second chunk and its link to the old third
		// page; the only invalid property is the new tail's old-ceiling escape.
		let suffix_number = old_next.assert_some();
		let suffix_key = PageKey::new(files.space(), files.cross_level_adjacency, suffix_number);
		let (suffix_next, suffix) = decode_cross_level_overflow(
			&store.read_update_page(&state, suffix_key).assert_ok(),
			suffix_number,
		)
		.assert_ok();
		assert!(suffix_next.is_some());
		let tail =
			encode_cross_level_overflow(tail_key.page_no(), PageLsn::ZERO, suffix_next, &suffix)
				.assert_ok();
		stage(store, tail_key, &tail);
		assert_eq!(
			decode_cross_level_overflow(
				&store.read_update_page(&state, tail_key).assert_ok(),
				tail_key.page_no()
			)
			.assert_ok(),
			(suffix_next, suffix)
		);
	}
	(key, original)
}

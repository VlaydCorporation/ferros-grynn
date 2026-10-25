//! A checksummed physical payload cannot replace the accepted source image.

use std::collections::BTreeSet;

use super::{label_lifecycle_reuse_tests::attach, *};
use crate::{
	datastore::PublicationDelta,
	gql_status::Status,
	storage::{
		atom_page::PropertyRef,
		atom_store_image::value_context,
		graph_pages::{
			EdgeSlotFlags,
			GraphOverlayStore,
			NodeSlotFlags,
			encode_overlay,
			hot_slot_address,
			read_edge_hot_slot,
			read_node_hot_slot,
			write_edge_hot_slot,
			write_node_hot_slot,
		},
		local_store::{point_graph::GraphUpdatePlan, point_incidence, point_namespace::LabelPlan},
		page::{AlignedPage, PAGE_HEADER_SIZE, PageFlags, PageLsn},
		types::PageKey,
	},
};

/// Distinct trust boundaries retain checksums while violating source semantics.
#[derive(Clone, Copy, Debug)]
enum SourceFault {
	/// Correct length and framing contain a different accepted value.
	Bytes,
	/// The Hot slot omits a payload that the accepted occurrence still owns.
	Missing,
	/// Valid WAL-visible bytes lie beyond the old graph header's ceiling.
	Ceiling,
	/// Inline directory or large-chain metadata is malformed.
	Layout,
	/// Known flag bits are valid for a page but invalid for its payload format.
	Flags,
	/// Reserved payload metadata must stay zero even with a valid checksum.
	Padding,
}

/// Checks prepared keys before the WAL staging map can replace duplicates.
#[test]
fn overlay_lifecycle_mixed_plan_has_one_header_and_contiguous_append_range() {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		let (fixture, pending) = membership_fixture(IoMode::Sync);
		let graph = fixture.graph.id();
		let local = local_store(&fixture.store);
		let published = fixture.store.inner.current_root();
		let mut before = published.as_ref().clone();
		local.hydrate_statistics(&mut before).assert_ok();
		// Cold explicit indexes must be ready before direct root mutation.
		before.rebuild_indexes().assert_ok();
		let record = &pending[usize::from(kind == AtomKind::Edge)];
		let global = before.resolve_atom(record).assert_some();
		let stable = before.resolve_atom(&fixture.records[0]).assert_some();
		let mut after = before.clone();
		let atom = match kind {
			AtomKind::Node => after.attach_node(graph, global, Some("A".into()), None).assert_ok(),
			AtomKind::Edge => after
				.attach_edge(
					graph,
					global,
					Some("A".into()),
					EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]]),
					None,
					None,
				)
				.assert_ok(),
		};
		let stable_atom = after.membership(graph, stable).assert_some();
		for (id, byte) in [(atom, 7), (stable_atom, 9)] {
			after
				.set_overlay(graph, id, fixture.keys[1], Value::Bytes(vec![byte; 40_000].into()))
				.assert_ok();
		}
		let delta = PublicationDelta {
			records: [record.clone(), fixture.records[0].clone()].into_iter().collect(),
			global_slots: BTreeSet::new(),
			overlays: [(graph, global), (graph, stable)].into_iter().collect(),
			incidence: [(graph, atom)].into_iter().collect(),
			memberships: [(graph, global)].into_iter().collect(),
			point_graph_only: true,
		};
		assert!(point_incidence::eligible(&before, &after, &delta));
		let state = local.state.lock();
		let files = state
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == graph)
			.assert_some()
			.files;
		let start = local.visible_pages(&state, files.space(), files.overlays).assert_ok();
		let boundary = local.wal.snapshot().visible_lsn();
		let mut labels = LabelPlan::build(local, &state, &before, &after, &delta).assert_ok();
		let mut context =
			value_context(&after.schema, after.dictionary.as_ref().clone()).assert_ok();
		let plan = GraphUpdatePlan::build(
			local,
			&state,
			&before,
			&after,
			&delta,
			&mut labels,
			&mut context,
		)
		.assert_ok();
		let keys =
			plan.pages.iter().map(|(key, _)| *key).collect::<std::collections::BTreeSet<_>>();
		assert_eq!(keys.len(), plan.pages.len());
		assert_eq!(plan.superblocks.len(), 1);
		assert_eq!(
			plan.pages.iter().filter(|(key, _)| key.file_id() == files.superblock).count(),
			1
		);
		let mut appended = plan
			.pages
			.iter()
			.filter(|(key, _)| key.file_id() == files.overlays)
			.map(|(key, _)| key.page_no().get())
			.collect::<Vec<_>>();
		appended.sort_unstable();
		assert_eq!(appended, (start..plan.superblocks[&graph].overlays_pages).collect::<Vec<_>>());
		assert!(appended.len() > 1);
		if kind == AtomKind::Node {
			assert_eq!(
				hot_slot_address(atom.slot()).assert_ok().0,
				hot_slot_address(stable_atom.slot()).assert_ok().0
			);
		}
		let mut refs = Vec::new();
		for id in [atom, stable_atom] {
			let (number, within) = hot_slot_address(id.slot()).assert_ok();
			let file = if id.is_node() {
				files.nodes
			} else {
				files.edges
			};
			let key = PageKey::new(files.space(), file, number);
			let page = &plan.pages.iter().find(|(candidate, _)| *candidate == key).assert_some().1;
			refs.push(if id.is_node() {
				read_node_hot_slot(page, number, within).assert_ok().overlay
			} else {
				read_edge_hot_slot(page, number, within).assert_ok().overlay
			});
		}
		assert_ne!(refs[0], refs[1]);
		assert!(refs.iter().all(|reference| matches!(reference, PropertyRef::Large { .. })));
		assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
		assert!(Arc::ptr_eq(&fixture.store.inner.current_root(), &published));
	}
}

/// NODE and EDGE detach validate inline/large sources before publication.
#[test]
fn overlay_lifecycle_false_source_and_old_ceiling_publish_nothing() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		for kind in [AtomKind::Node, AtomKind::Edge] {
			for length in [64, 40_000] {
				for fault in [
					SourceFault::Bytes,
					SourceFault::Missing,
					SourceFault::Ceiling,
					SourceFault::Layout,
					SourceFault::Flags,
					SourceFault::Padding,
				] {
					false_source(io_mode, kind, length, fault);
				}
			}
		}
	}
}

/// Direct preparation isolates a false physical source from the logical clock.
fn false_source(io_mode: IoMode, kind: AtomKind, length: usize, fault: SourceFault) {
	let (mut fixture, pending) = membership_fixture(io_mode);
	let graph = fixture.graph.id();
	let record = &pending[usize::from(kind == AtomKind::Edge)];
	let mut seed = fixture.store.begin();
	attach(&mut seed, &fixture, record, kind, "M");
	seed.set_overlay(graph, record, fixture.keys[1], Value::Bytes(vec![7; length].into()))
		.assert_ok();
	seed.commit().assert_ok();
	fixture = reopen_checkpointed(fixture, io_mode);
	let local = local_store(&fixture.store);
	let published = fixture.store.inner.current_root();
	let mut before = published.as_ref().clone();
	local.hydrate_statistics(&mut before).assert_ok();
	// Cold explicit indexes must be ready before direct root mutation.
	before.rebuild_indexes().assert_ok();
	let global = before.resolve_atom(record).assert_some();
	let atom = before.membership(graph, global).assert_some();
	let mut after = before.clone();
	after.detach(graph, global).assert_ok().assert_some();
	let delta = PublicationDelta {
		records: [record.clone()].into_iter().collect(),
		global_slots: BTreeSet::new(),
		overlays: BTreeSet::new(),
		incidence: [(graph, atom)].into_iter().collect(),
		memberships: [(graph, global)].into_iter().collect(),
		point_graph_only: true,
	};
	assert!(point_incidence::eligible(&before, &after, &delta));
	let state = local.state.lock();
	let files =
		state.bindings.graphs.iter().find(|binding| binding.graph_id == graph).assert_some().files;
	let mut labels = LabelPlan::build(local, &state, &before, &after, &delta).assert_ok();
	let mut context = value_context(&after.schema, after.dictionary.as_ref().clone()).assert_ok();
	GraphUpdatePlan::build(local, &state, &before, &after, &delta, &mut labels, &mut context)
		.assert_ok();
	let (number, within) = hot_slot_address(atom.slot()).assert_ok();
	let hot_key = PageKey::new(
		files.space(),
		if atom.is_node() {
			files.nodes
		} else {
			files.edges
		},
		number,
	);
	let mut hot = local.read_update_page(&state, hot_key).assert_ok();
	let old_ref = if atom.is_node() {
		read_node_hot_slot(&hot, number, within).assert_ok().overlay
	} else {
		read_edge_hot_slot(&hot, number, within).assert_ok().overlay
	};
	let old_first = match old_ref {
		PropertyRef::Inline {
			page,
			slot,
			..
		} => {
			assert_eq!(slot, 0);
			Some(page.get())
		}
		PropertyRef::Large {
			first_page,
			..
		} => Some(first_page.get()),
		PropertyRef::None => None,
	}
	.assert_some();
	let first = if matches!(fault, SourceFault::Ceiling) {
		state.graph_superblocks[&graph].overlays_pages
	} else {
		old_first
	};
	let mut encoding_root = before.clone();
	if matches!(fault, SourceFault::Bytes) {
		encoding_root
			.set_overlay(graph, atom, fixture.keys[1], Value::Bytes(vec![9; length].into()))
			.assert_ok();
	}
	let overlay = encoding_root.occurrence(graph, atom).assert_some().overlay().assert_some();
	let mut context = value_context(&before.schema, before.dictionary.as_ref().clone()).assert_ok();
	let encoded = encode_overlay(overlay, &mut context).assert_ok();
	let mut builder = GraphOverlayStore::at_page(first);
	let target_ref = builder.insert(&encoded, PageLsn::ZERO).assert_ok();
	if !matches!(fault, SourceFault::Ceiling) {
		assert_eq!(target_ref, old_ref);
	}
	let mut pages = builder.finish().assert_ok();
	corrupt_payload_page(&mut pages[0], old_ref, fault);
	let target_ref = if matches!(fault, SourceFault::Missing) {
		PropertyRef::None
	} else {
		target_ref
	};
	if atom.is_node() {
		let mut slot = read_node_hot_slot(&hot, number, within).assert_ok();
		slot.overlay = target_ref;
		slot.flags.set(NodeSlotFlags::HAS_OVERLAY, target_ref != PropertyRef::None);
		write_node_hot_slot(&mut hot, within, &slot).assert_ok();
	} else {
		let mut slot = read_edge_hot_slot(&hot, number, within).assert_ok();
		slot.overlay = target_ref;
		slot.flags.set(EdgeSlotFlags::HAS_OVERLAY, target_ref != PropertyRef::None);
		write_edge_hot_slot(&mut hot, within, &slot).assert_ok();
	}
	hot.seal().assert_ok();
	let mut injection = local.wal.begin_tx().assert_ok();
	injection.stage_page(hot_key, &hot).assert_ok();
	for (index, page) in pages.iter().enumerate() {
		let number = crate::storage::page::PageNo::new(first + u64::try_from(index).assert_ok());
		injection.stage_page(PageKey::new(files.space(), files.overlays, number), page).assert_ok();
	}
	local.wal.commit_tx(injection).assert_ok();
	if matches!(fault, SourceFault::Ceiling) {
		assert!(local.visible_pages(&state, files.space(), files.overlays).assert_ok() > first);
	}
	let boundary = local.wal.snapshot().visible_lsn();
	let catalog = state.file_catalog.files.clone();
	let directories = generation_directories(&fixture.directory.0);
	let mut labels = LabelPlan::build(local, &state, &before, &after, &delta).assert_ok();
	let mut context = value_context(&after.schema, after.dictionary.as_ref().clone()).assert_ok();
	let error =
		GraphUpdatePlan::build(local, &state, &before, &after, &delta, &mut labels, &mut context)
			.err()
			.assert_some();
	let expected = match fault {
		SourceFault::Bytes => "bytes disagree with replay",
		SourceFault::Missing => "overlay reference is absent",
		SourceFault::Ceiling => "header ceiling",
		SourceFault::Layout if length < 100 => "page metadata is inconsistent",
		SourceFault::Layout => "chain link is inconsistent",
		SourceFault::Flags if length < 100 => "page type or flags",
		SourceFault::Flags => "large-property page header",
		SourceFault::Padding => "padding are non-zero",
	};
	let rejected = match (fault, error.status().status()) {
		(
			SourceFault::Layout | SourceFault::Flags | SourceFault::Padding,
			Status::InvalidStoragePage {
				reason,
				..
			},
		) => reason.contains(expected),
		(
			SourceFault::Bytes | SourceFault::Missing | SourceFault::Ceiling,
			Status::InvalidStorageFile {
				reason,
			},
		) => reason.contains(expected),
		_ => false,
	};
	assert!(rejected, "{io_mode:?}/{kind:?}/{length}/{fault:?}: {error}");
	assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
	assert_eq!(state.file_catalog.files, catalog);
	assert_eq!(generation_directories(&fixture.directory.0), directories);
	assert!(Arc::ptr_eq(&fixture.store.inner.current_root(), &published));
}

/// Corrupts format metadata independently of fixture and publication setup.
fn corrupt_payload_page(page: &mut AlignedPage, reference: PropertyRef, fault: SourceFault) {
	if matches!(fault, SourceFault::Layout | SourceFault::Flags | SourceFault::Padding) {
		// The explicit format fields are corrupted, then the checksum is
		// repaired.
		let bytes = page.as_bytes_mut();
		if matches!(fault, SourceFault::Flags) {
			// The format-v1 common flags field follows magic/u16 page type.
			let flags = if matches!(reference, PropertyRef::Inline { .. }) {
				PageFlags::OVERFLOW.bits()
			} else {
				0
			};
			bytes[6..8].copy_from_slice(&flags.to_le_bytes());
		} else if matches!(fault, SourceFault::Padding) {
			let reserved = if matches!(reference, PropertyRef::Inline { .. }) {
				PAGE_HEADER_SIZE + 6
			} else {
				PAGE_HEADER_SIZE + 12
			};
			bytes[reserved] = 1;
		} else if matches!(reference, PropertyRef::Inline { .. }) {
			bytes[PAGE_HEADER_SIZE + 2..PAGE_HEADER_SIZE + 4].fill(0);
		} else {
			bytes[PAGE_HEADER_SIZE..PAGE_HEADER_SIZE + 8].fill(0);
		}
		page.seal().assert_ok();
	}
}

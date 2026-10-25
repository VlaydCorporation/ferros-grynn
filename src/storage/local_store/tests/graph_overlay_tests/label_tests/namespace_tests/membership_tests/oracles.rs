//! Decode complete physical records before normalizing append addresses.

use super::*;
use crate::{
	storage::{
		PhysicalIndexEntries,
		graph_index::{ScalarIndexMode, scan_scalar_index},
		graph_pages::{
			build_graph_image,
			decode_class_statistics,
			decode_label_statistics,
			decode_statistics_root,
			hot_slot_address,
			read_edge_hot_slot,
			read_node_hot_slot,
		},
		page::{PageLsn, PageNo},
		types::{DataFileKind, PageKey},
	},
	values::storage::{DiskAtomRef, EncodeContext},
};

/// Small fixtures have inline adjacency; overlay payloads are checked exactly.
pub(super) fn assert_image(fixture: &LabelFixture) {
	let root = fixture.store.inner.current_root();
	let graph = fixture.graph.id();
	let local = local_store(&fixture.store);
	let physical = local.capture_query_snapshot(root.commit_ts).assert_ok().assert_some();
	let context = crate::datastore::index_context(&root.schema, &root.dictionary).assert_ok();
	for occurrence in root.graph(graph).assert_some().occurrences() {
		let fields = local
			.physical_traversal_fields(&physical, graph, occurrence.local_id(), &context.decoder())
			.assert_ok()
			.assert_some();
		let base = root.atom(occurrence.global_id()).assert_some();
		assert_eq!(fields.global, occurrence.global_id());
		assert_eq!(&fields.base_properties, base.properties());
		assert_eq!(fields.overlay.as_ref(), occurrence.overlay());
	}
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
			(header.node_capacity, header.edge_capacity, header.node_count, header.edge_count),
			(
				bulk.superblock.node_capacity,
				bulk.superblock.edge_capacity,
				bulk.superblock.node_count,
				bulk.superblock.edge_count
			)
		);
		assert_eq!(
			(header.checkpoint_generation, header.backfilled_lsn, header.checkpoint_record_lsn),
			(
				state.database_superblock.generation,
				state.database_superblock.checkpoint_lsn,
				state.database_superblock.checkpoint_record_lsn
			)
		);
		for slot in 0..header.node_capacity {
			let (number, within) = hot_slot_address(slot).assert_ok();
			let page = local
				.read_update_page(&state, PageKey::new(files.space(), files.nodes, number))
				.assert_ok();
			let actual = read_node_hot_slot(&page, number, within).assert_ok();
			let mut expected = read_node_hot_slot(
				&bulk.nodes[usize::try_from(number.get()).assert_ok()],
				number,
				within,
			)
			.assert_ok();
			assert!(
				actual.out_overflow.is_none()
					&& actual.in_overflow.is_none()
					&& actual.undirected_overflow.is_none()
			);
			// Exact fields above prove the payload before append offsets
			// differ.
			expected.overlay = actual.overlay;
			assert_eq!(actual, expected);
		}
		for slot in 0..header.edge_capacity {
			let (number, within) = hot_slot_address(slot).assert_ok();
			let page = local
				.read_update_page(&state, PageKey::new(files.space(), files.edges, number))
				.assert_ok();
			let actual = read_edge_hot_slot(&page, number, within).assert_ok();
			let mut expected = read_edge_hot_slot(
				&bulk.edges[usize::try_from(number.get()).assert_ok()],
				number,
				within,
			)
			.assert_ok();
			expected.overlay = actual.overlay;
			assert_eq!(actual, expected);
		}
	}
	assert_label_arrays(&fixture.store, graph);
	let class = root.schema.class("item").assert_some();
	let mut checked = 0;
	for index in class.indexes() {
		checked += 1;
		let scoped = matches!(index.scope(), IndexScope::Effective { .. });
		let rows = root
			.atoms
			.atoms()
			.filter(|atom| Some(atom.class_id()) == class.id())
			.filter(|atom| !scoped || root.membership(graph, atom.id()).is_some())
			.collect::<Vec<_>>();
		let decoded =
			local.physical_index(&physical, index.id().assert_some()).assert_ok().assert_some();
		match &decoded.entries {
			PhysicalIndexEntries::Count(count) => {
				assert_eq!(*count, u64::try_from(rows.len()).assert_ok());
			}
			PhysicalIndexEntries::Scalar(groups) => {
				assert_eq!(groups.iter().map(|group| group.atoms.len()).sum::<usize>(), rows.len());
				let mut expected = BTreeMap::<Vec<u8>, Vec<(AtomKind, u64)>>::new();
				for atom in rows {
					let owner = if scoped {
						root.membership(graph, atom.id()).assert_some()
					} else {
						AtomId::from_raw(atom.id().raw())
					};
					let value = if scoped {
						root.effective_property(graph, owner, fixture.keys[0])
							.assert_ok()
							.cloned()
							.assert_some()
					} else {
						atom.properties().get(fixture.keys[0]).cloned().assert_some()
					};
					let key = crate::values::storage::encode_index_key(
						&[(&value, &Type::Int)],
						&mut context,
					)
					.assert_ok();
					expected
						.entry(key)
						.or_default()
						.push((owner.kind().assert_some(), owner.slot()));
				}
				for owners in expected.values_mut() {
					owners.sort_unstable();
				}
				let actual = groups
					.iter()
					.map(|group| {
						(
							group.key.clone(),
							group
								.atoms
								.iter()
								.map(|owner| (owner.kind, owner.slot))
								.collect::<Vec<_>>(),
						)
					})
					.collect::<BTreeMap<_, _>>();
				assert_eq!(actual.len(), groups.len());
				assert_eq!(actual, expected);
			}
			_ => unreachable!("mixed fixture contains only scalar and Count indexes"),
		}
	}
	assert_eq!(checked, 4);
	assert_statistics(fixture);
	assert_scoped_addresses(fixture);
	crate::storage::local_store::tests::base_lifecycle_tests::assert_base_raw_postings(
		&fixture.store,
	);
}

/// Decodes WAL-visible counters directly, without an optional lazy binding.
fn assert_statistics(fixture: &LabelFixture) {
	let root = fixture.store.inner.current_root();
	let graph = fixture.graph.id();
	let mut labels =
		BTreeMap::<crate::stats::LabelStatisticKey, crate::stats::LabelStatistic>::new();
	let mut classes = BTreeMap::new();
	for occurrence in root.graph(graph).assert_some().occurrences() {
		let base = root.atom(occurrence.global_id()).assert_some();
		*classes.entry(base.class_id()).or_insert(0_u64) += 1;
		let Some(label) = occurrence.label() else {
			continue;
		};
		let mut keys =
			base.properties().iter().map(|(key, _)| key).collect::<std::collections::BTreeSet<_>>();
		if let Some(overlay) = occurrence.overlay() {
			for (key, value) in overlay.iter() {
				match value {
					crate::datastore::OverlayValue::Hidden => {
						keys.remove(&key);
					}
					crate::datastore::OverlayValue::Value(_) => {
						keys.insert(key);
					}
				}
			}
		}
		let counter = labels
			.entry(crate::stats::LabelStatisticKey {
				kind: occurrence.local_id().kind().assert_some(),
				label: label.clone(),
			})
			.or_default();
		counter.atom_count += 1;
		counter.property_count += u64::try_from(keys.len()).assert_ok();
	}
	let local = local_store(&fixture.store);
	let state = local.state.lock();
	let space = DataSpaceId::Graph(graph);
	let statistics =
		LocalStore::statistics_file(&state, space, DataFileKind::Statistics).assert_ok();
	let label_file =
		LocalStore::statistics_file(&state, space, DataFileKind::LabelStatistics).assert_ok();
	let property_file =
		LocalStore::statistics_file(&state, space, DataFileKind::PropertyStatistics).assert_ok();
	let page =
		local.read_update_page(&state, PageKey::new(space, statistics, PageNo::new(0))).assert_ok();
	let (physical, layout) = decode_statistics_root(
		&page,
		local.visible_pages(&state, space, statistics).assert_ok(),
		local.visible_pages(&state, space, label_file).assert_ok(),
		local.visible_pages(&state, space, property_file).assert_ok(),
	)
	.assert_ok();
	let dictionary = LabelDictionary::from_labels(
		root.graph(graph).assert_some().occurrences().filter_map(|occurrence| {
			let kind = match occurrence.local_id().kind()? {
				AtomKind::Node => LabelKind::Node,
				AtomKind::Edge => LabelKind::Edge,
			};
			Some((kind, occurrence.label()?.as_str()))
		}),
	)
	.assert_ok();
	let actual_labels = decode_label_statistics(
		|number, output| {
			local.read_update_page_into(&state, PageKey::new(space, label_file, number), output)
		},
		layout,
		&dictionary,
	)
	.assert_ok();
	let actual_classes = decode_class_statistics(
		|number, output| {
			local.read_update_page_into(&state, PageKey::new(space, statistics, number), output)
		},
		layout,
	)
	.assert_ok();
	assert_eq!(actual_labels, labels);
	assert_eq!(actual_classes, classes);
	assert_eq!(physical.revision, root.statistics.graph(graph).assert_some().revision);
	for (kind, count) in
		[(AtomKind::Node, physical.node_count), (AtomKind::Edge, physical.edge_count)]
	{
		let expected = root
			.graph(graph)
			.assert_some()
			.occurrences()
			.filter(|occurrence| occurrence.local_id().kind() == Some(kind))
			.count();
		assert_eq!(count, u64::try_from(expected).assert_ok());
	}
}

/// Scoped codecs retain kind/slot addresses; Hot records carry generations.
fn assert_scoped_addresses(fixture: &LabelFixture) {
	let root = fixture.store.inner.current_root();
	let graph = fixture.graph.id();
	let local = local_store(&fixture.store);
	let state = local.state.lock();
	let mut context = crate::datastore::index_context(&root.schema, &root.dictionary).assert_ok();
	for binding in root.indexes.graphs[&graph]
		.explicit
		.values()
		.filter(|binding| binding.kind != ClassIndexKind::Count)
	{
		let physical = state.bindings.physical_indexes[&binding.id];
		let mode = match binding.kind {
			ClassIndexKind::Standard => ScalarIndexMode::Standard,
			ClassIndexKind::Unique => ScalarIndexMode::Unique,
			_ => unreachable!("mixed fixture has a non-scalar Effective index"),
		};
		let entries = scan_scalar_index::<DiskAtomRef>(
			|number, output| {
				local.read_update_page_into(
					&state,
					PageKey::new(DataSpaceId::Graph(graph), physical.file_id, number),
					output,
				)
			},
			physical.root,
			mode,
		)
		.assert_ok();
		let mut actual = BTreeMap::new();
		for entry in entries {
			let count = entry.atoms.len();
			let owners = entry.atoms.into_iter().collect::<std::collections::BTreeSet<_>>();
			assert_eq!(owners.len(), count);
			assert!(actual.insert(entry.key, owners).is_none());
		}
		let mut expected = BTreeMap::<_, std::collections::BTreeSet<_>>::new();
		for occurrence in root.graph(graph).assert_some().occurrences().filter(|occurrence| {
			root.atom(occurrence.global_id()).assert_some().class_id() == binding.class_id
		}) {
			let value = root
				.effective_property(graph, occurrence.local_id(), fixture.keys[0])
				.assert_ok()
				.assert_some();
			let key =
				crate::values::storage::encode_index_key(&[(value, &Type::Int)], &mut context)
					.assert_ok();
			expected
				.entry(key)
				.or_default()
				.insert(local_address(occurrence.local_id()).assert_ok());
		}
		assert_eq!(actual, expected);
	}
}

//! Existing-slot scalar state changes must retain physical graph identities.

use super::*;
use crate::{
	datastore::{OccurrenceState, OccurrenceStateRef},
	graph::{AtomId, GraphId, GraphRef},
	storage::{
		graph_pages::{decode_statistics_root, hot_slot_address, read_edge_hot_slot},
		image_io::read_page_cached,
		local_store::LocalStore,
		page::AlignedPage,
		types::{DataFileKind, PageKey, StorageSnapshot},
	},
	values::Number,
};

/// Existing edge slots and one scalar binding shared by durability scenarios.
struct WeightFixture {
	/// Cold-open store with its maintenance worker stopped.
	store: Datastore,
	/// Requested backend mode retained across reopen.
	options: LocalOptions,
	/// Existing graph identity.
	graph: GraphRef,
	/// Indexed Base/Effective property.
	payload: PropertyKeyId,
	/// Global edge records in stable slot order.
	records: [RecordId; 3],
	/// Generation-aware local edge handles.
	edges: [AtomId; 3],
	/// Fixed endpoints reused by weight-only changes.
	topology: EdgeTopology,
	/// Fields drop in declaration order: files must close before directory
	/// cleanup.
	directory: TestDirectory,
}

/// Sync weight changes do not rebuild unaffected graph and atom files.
#[test]
fn state_point_sync_weight_preserves_pages_and_reopens() {
	weight_publication(IoMode::Sync);
}

/// Actual NativeAsync shares the same weight/overlay WAL publication.
#[cfg(feature = "native-async-io")]
#[test]
fn state_point_native_async_weight_preserves_pages_and_reopens() {
	weight_publication(IoMode::NativeAsync);
}

/// Reads exact WAL/home EdgeHot versions independently of the runtime state.
fn assert_physical_weights<const N: usize>(
	local: &crate::storage::local_store::LocalStore,
	graph: GraphId,
	edges: &[AtomId; N],
	snapshot: StorageSnapshot,
	expected: [Option<f64>; N],
) {
	let state = local.state.lock();
	let files =
		state.bindings.graphs.iter().find(|binding| binding.graph_id == graph).assert_some().files;
	for (edge, expected) in edges.iter().zip(expected) {
		let (number, within) = hot_slot_address(edge.slot()).assert_ok();
		let mut page = Box::new(AlignedPage::zeroed());
		read_page_cached(
			&local.wal,
			&state.registry,
			&local.engine,
			snapshot,
			state.database_superblock.generation,
			state.database_superblock.checkpoint_lsn,
			PageKey::new(crate::datastore::DataSpaceId::Graph(graph), files.edges, number),
			&mut page,
		)
		.assert_ok();
		let slot = read_edge_hot_slot(&page, number, within).assert_ok();
		assert_eq!(slot.generation, edge.generation());
		assert_eq!(slot.weight(), expected);
	}
}

/// Compares eager statistics and catalog clocks against the accepted root.
fn assert_weight_metadata(store: &Datastore, graph: GraphId) {
	let root = store.inner.current_root();
	let local = match &store.inner.backend {
		StoreBackend::Local(local) => Some(local),
		StoreBackend::InMemory => None,
	}
	.assert_some();
	let state = local.state.lock();
	let space = crate::datastore::DataSpaceId::Graph(graph);
	let statistics_file =
		LocalStore::statistics_file(&state, space, DataFileKind::Statistics).assert_ok();
	let label_file =
		LocalStore::statistics_file(&state, space, DataFileKind::LabelStatistics).assert_ok();
	let property_file =
		LocalStore::statistics_file(&state, space, DataFileKind::PropertyStatistics).assert_ok();
	let page = local
		.read_update_page(
			&state,
			PageKey::new(space, statistics_file, crate::storage::page::PageNo::new(0)),
		)
		.assert_ok();
	let (physical, _) = decode_statistics_root(
		&page,
		local.visible_pages(&state, space, statistics_file).assert_ok(),
		local.visible_pages(&state, space, label_file).assert_ok(),
		local.visible_pages(&state, space, property_file).assert_ok(),
	)
	.assert_ok();
	let statistics = root.statistics.graph(graph).assert_some();
	assert_eq!(physical.revision, statistics.revision);
	assert_eq!(physical.dirty_rows, statistics.dirty_rows);
	let entry = state._database_catalog.graphs.iter().find(|entry| entry.id == graph).assert_some();
	let binding = state.bindings.graphs.iter().find(|entry| entry.graph_id == graph).assert_some();
	assert_eq!(entry.tensor_revision, root.graph(graph).assert_some().tensor_revision);
	assert_eq!(entry.generation, binding.generation);
	assert!(entry.tensor_revision <= entry.generation);
}

/// Seeds and cold-opens a scalar-indexed graph with three unweighted edges.
fn weight_fixture(io_mode: IoMode) -> WeightFixture {
	let directory = TestDirectory::new();
	let mut options = LocalOptions {
		io_mode,
		buffer_pool_pages: 64,
		..LocalOptions::default()
	};
	options.checkpoint.unbackfilled_page_percent = 100;
	options.checkpoint.interval = Duration::from_secs(3600);
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	let mut seed = store.begin();
	let graph = seed.define_graph("g", GraphKind::Graph).assert_ok();
	seed.define_class(ClassSchema::new("vertex", AtomKind::Node, []).assert_ok()).assert_ok();
	let class =
		ClassSchema::new("link", AtomKind::Edge, [ClassField::new("payload", Type::String)])
			.assert_ok()
			.with_index(
				ClassIndex::new(
					"effective",
					ClassIndexKind::Standard,
					IndexScope::Effective {
						graph: graph.id(),
					},
					["payload"],
				)
				.assert_ok(),
			)
			.assert_ok();
	seed.define_class(class).assert_ok();
	let payload = seed.intern_property("payload").assert_ok();
	let mut nodes = Vec::new();
	for name in ["vertex:a", "vertex:b"] {
		let record = name.parse::<RecordId>().assert_ok();
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		nodes.push(seed.attach_node(graph.id(), &record, None, None).assert_ok());
	}
	let topology = EdgeTopology::directed([nodes[0]], [nodes[1]]);
	let records = ["link:a", "link:b", "link:c"].map(|name| name.parse::<RecordId>().assert_ok());
	let mut edge_ids = Vec::new();
	for record in &records {
		let mut properties = PropertyMap::new();
		properties.insert(payload, Value::from("base"));
		seed.create_atom(record.clone(), properties).assert_ok();
		edge_ids.push(
			seed.attach_edge(graph.id(), record, Some("E".into()), topology.clone(), None, None)
				.assert_ok(),
		);
	}
	let edges: [AtomId; 3] = edge_ids.try_into().assert_ok();
	seed.commit().assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	store.inner.maintenance.shutdown();
	WeightFixture {
		store,
		options,
		graph,
		payload,
		records,
		edges,
		topology,
		directory,
	}
}

/// Covers grouped state patches, mixed overlays and old snapshots across
/// checkpoint.
fn weight_publication(io_mode: IoMode) {
	let WeightFixture {
		directory,
		store,
		options,
		graph,
		payload,
		records,
		edges,
		topology,
	} = weight_fixture(io_mode);
	let local = match &store.inner.backend {
		StoreBackend::Local(local) => Some(local),
		StoreBackend::InMemory => None,
	}
	.assert_some();
	let mut source = store.begin();
	source.set_base_property(&records[0], payload, "source".into()).assert_ok();
	source.commit().assert_ok();
	let files = local.state.lock().file_catalog.files.clone();
	let old = store.begin();
	let old_root = store.inner.current_root();
	let old_pin = local.capture_query_snapshot(old_root.commit_ts).assert_ok().assert_some();
	let old_snapshot = local.wal.snapshot();
	let overlay_ceiling = local.state.lock().graph_superblocks[&graph.id()].overlays_pages;
	let boundary = local.wal.snapshot().visible_lsn();
	let mut weight = store.begin();
	for (record, value) in records[..2].iter().zip([1_i64, 2]) {
		weight
			.replace_occurrence_state(
				graph.id(),
				record,
				OccurrenceState::edge(topology.clone(), Some(value.into()), None),
			)
			.assert_ok();
	}
	weight.commit().assert_ok();
	assert_eq!(local.state.lock().file_catalog.files, files);
	assert_weight_metadata(&store, graph.id());
	assert_eq!(local.state.lock().graph_superblocks[&graph.id()].overlays_pages, overlay_ceiling);
	let frames = local
		.wal
		.recovery_frames()
		.into_iter()
		.filter(|frame| frame.frame_lsn() > boundary)
		.collect::<Vec<_>>();
	assert_eq!(
		frames
			.iter()
			.filter(|frame| frame.key().file_id().kind() == Some(DataFileKind::EdgesHot))
			.count(),
		1
	);
	assert!(frames.iter().all(|frame| matches!(
		frame.key().file_id().kind(),
		Some(
			DataFileKind::EdgesHot
				| DataFileKind::GraphSuperblock
				| DataFileKind::Statistics
				| DataFileKind::SystemCatalog
		)
	)));
	assert_physical_weights(local, graph.id(), &edges, local.wal.snapshot(), [
		Some(1.0),
		Some(2.0),
		None,
	]);
	let mut mixed = store.begin();
	mixed
		.replace_occurrence_state(
			graph.id(),
			&records[0],
			OccurrenceState::edge(topology.clone(), Some(3_i64.into()), None),
		)
		.assert_ok();
	mixed
		.replace_occurrence_state(
			graph.id(),
			&records[1],
			OccurrenceState::edge(topology.clone(), None, None),
		)
		.assert_ok();
	mixed.set_overlay(graph.id(), &records[0], payload, "overlay".into()).assert_ok();
	mixed.hide_property(graph.id(), &records[2], payload).assert_ok();
	mixed.commit().assert_ok();
	assert_eq!(local.state.lock().file_catalog.files, files);
	assert_weight_metadata(&store, graph.id());
	assert_physical_weights(local, graph.id(), &edges, local.wal.snapshot(), [
		Some(3.0),
		None,
		None,
	]);
	assert_overlay_property(&store, graph.id(), &records[0], payload, "overlay".into());
	assert_overlay_label_statistic(&store, graph.id(), AtomKind::Edge, "E", 3, 2);
	// Net-zero fields still advance incidence/statistics revisions, without a
	// Hot patch. A later overlay-only commit must retain those catalog clocks.
	let boundary = local.wal.snapshot().visible_lsn();
	let mut net_zero = store.begin();
	for weight in [Some(4_i64.into()), None] {
		net_zero
			.replace_occurrence_state(
				graph.id(),
				&records[1],
				OccurrenceState::edge(topology.clone(), weight, None),
			)
			.assert_ok();
	}
	net_zero.commit().assert_ok();
	assert!(
		local
			.wal
			.recovery_frames()
			.into_iter()
			.filter(|frame| frame.frame_lsn() > boundary)
			.all(|frame| frame.key().file_id().kind() != Some(DataFileKind::EdgesHot))
	);
	assert_weight_metadata(&store, graph.id());
	let mut overlay_only = store.begin();
	overlay_only.set_overlay(graph.id(), &records[0], payload, "overlay".into()).assert_ok();
	overlay_only.commit().assert_ok();
	assert_weight_metadata(&store, graph.id());
	store.checkpoint(CheckpointMode::Passive).assert_ok();
	assert!(local.state.lock().database_superblock.checkpoint_lsn >= old_snapshot.visible_lsn());
	assert_physical_weights(local, graph.id(), &edges, old_snapshot, [None; 3]);
	assert!(matches!(
		old_root.occurrence(graph.id(), edges[0]).assert_some().state().view(),
		OccurrenceStateRef::Edge {
			weight: None,
			..
		}
	));
	drop(old_pin);
	drop(old);
	drop(old_root);
	let stamp = store.inner.current_root().commit_ts;
	let boundary = local.wal.snapshot().visible_lsn();
	let statistics = store.inner.current_root().statistics.graph(graph.id()).assert_some().clone();
	let tensor_revision =
		store.inner.current_root().graph(graph.id()).assert_some().tensor_revision;
	let inexact = Number::from((1_i64 << 53) + 1);
	assert!(f64::try_from(inexact).is_err());
	let mut rejected = store.begin();
	rejected
		.replace_occurrence_state(
			graph.id(),
			&records[0],
			OccurrenceState::edge(topology, Some(inexact), None),
		)
		.assert_ok();
	rejected.commit().assert_err();
	assert_eq!(store.inner.current_root().commit_ts, stamp);
	assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
	assert_eq!(local.state.lock().file_catalog.files, files);
	drop(store);
	let reopened = Datastore::open_local(&directory.0, options).assert_ok();
	assert_eq!(reopened.local_storage_status().assert_some().options().io_mode, io_mode);
	assert_weight_metadata(&reopened, graph.id());
	let root = reopened.inner.current_root();
	let reopened_statistics = root.statistics.graph(graph.id()).assert_some();
	assert_eq!(reopened_statistics.revision, statistics.revision);
	assert_eq!(reopened_statistics.dirty_rows, statistics.dirty_rows);
	assert_eq!(root.graph(graph.id()).assert_some().tensor_revision, tensor_revision);
	let local = match &reopened.inner.backend {
		StoreBackend::Local(local) => Some(local),
		StoreBackend::InMemory => None,
	}
	.assert_some();
	assert_physical_weights(local, graph.id(), &edges, local.wal.snapshot(), [
		Some(3.0),
		None,
		None,
	]);
	assert_overlay_property(&reopened, graph.id(), &records[0], payload, "overlay".into());
	assert_overlay_label_statistic(&reopened, graph.id(), AtomKind::Edge, "E", 3, 2);
}

/// Mixed state/property failures are atomic in Sync mode.
#[test]
fn state_point_sync_failures_recover_one_complete_image() {
	weight_failures(IoMode::Sync);
}

/// NativeAsync preserves the same uncertain-writer recovery fence.
#[cfg(feature = "native-async-io")]
#[test]
fn state_point_native_async_failures_recover_one_complete_image() {
	weight_failures(IoMode::NativeAsync);
}

/// Same-edge independent fields survive either commit order in Sync mode.
#[test]
fn state_point_sync_independent_writers_preserve_both_fields() {
	weight_replay(IoMode::Sync);
}

/// Actual NativeAsync replays against the latest accepted Hot image.
#[cfg(feature = "native-async-io")]
#[test]
fn state_point_native_async_independent_writers_preserve_both_fields() {
	weight_replay(IoMode::NativeAsync);
}

/// Mixed ordinary structural changes preserve unaffected Sync file identities.
#[test]
fn state_point_sync_mixed_structural_changes_retain_unaffected_files() {
	weight_mixed_structural_publication(IoMode::Sync);
}

/// NativeAsync preserves requested mixed state and unaffected file identities.
#[cfg(feature = "native-async-io")]
#[test]
fn state_point_native_async_mixed_structural_changes_retain_unaffected_files() {
	weight_mixed_structural_publication(IoMode::NativeAsync);
}

/// A Ready weighted path or tensor must invalidate through the full publisher.
#[test]
fn state_point_sync_ready_artifacts_invalidate_atomically() {
	weight_artifact_fallback(IoMode::Sync);
}

/// NativeAsync preserves derived invalidation instead of forcing sparse
/// admission.
#[cfg(feature = "native-async-io")]
#[test]
fn state_point_native_async_ready_artifacts_invalidate_atomically() {
	weight_artifact_fallback(IoMode::NativeAsync);
}

/// Keeps Ready artifact removal, schema state and the source weight in one
/// commit.
fn weight_artifact_fallback(io_mode: IoMode) {
	use crate::datastore::{
		GraphIndex,
		GraphIndexCost,
		GraphIndexDefinition,
		PathIndexAlgorithm,
		TensorCacheFormat,
	};
	for definition in [
		GraphIndexDefinition::Path {
			algorithm: PathIndexAlgorithm::LandmarkSssp(2),
			cost: GraphIndexCost::Weighted,
			patterns: Vec::new(),
		},
		GraphIndexDefinition::Tensor {
			scope: crate::graph::tensor::TensorScope::Local,
			format: TensorCacheFormat::Csr,
		},
	] {
		let mut fixture = weight_fixture(io_mode);
		let mut define = fixture.store.begin();
		let index = define
			.define_graph_index(
				"g",
				GraphIndex::new("accelerated", definition, Some(1 << 20)).assert_ok(),
			)
			.assert_ok();
		define.commit().assert_ok();
		assert!(
			fixture
				.store
				.execute("REBUILD INDEX accelerated ON GRAPH g;", &Object::new())
				.assert_ok()[0]
				.result
				.is_ok()
		);
		let old = fixture.store.inner.current_root();
		assert_eq!(
			old.schema
				.graph(fixture.graph.id())
				.assert_some()
				.indexes()
				.next()
				.assert_some()
				.state(),
			ClassIndexState::Ready
		);
		assert!(
			old.path_artifacts.contains_key(&index) || old.tensor_artifacts.contains_key(&index)
		);
		let local = match &fixture.store.inner.backend {
			StoreBackend::Local(local) => Some(local),
			StoreBackend::InMemory => None,
		}
		.assert_some();
		let files = local.state.lock().file_catalog.files.clone();
		weight_candidate(&fixture).commit().assert_ok();
		assert_ne!(local.state.lock().file_catalog.files, files);
		let root = fixture.store.inner.current_root();
		assert_eq!(
			root.schema
				.graph(fixture.graph.id())
				.assert_some()
				.indexes()
				.next()
				.assert_some()
				.state(),
			ClassIndexState::Stale
		);
		assert!(
			!root.path_artifacts.contains_key(&index)
				&& !root.tensor_artifacts.contains_key(&index)
		);
		assert_weight_candidate(&fixture, true);
		drop(old);
		drop(root);
		drop(fixture.store);
		fixture.store = Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
		assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, io_mode);
		assert_weight_candidate(&fixture, true);
		let root = fixture.store.inner.current_root();
		assert_eq!(
			root.schema
				.graph(fixture.graph.id())
				.assert_some()
				.indexes()
				.next()
				.assert_some()
				.state(),
			ClassIndexState::Stale
		);
		assert!(
			!root.path_artifacts.contains_key(&index)
				&& !root.tensor_artifacts.contains_key(&index)
		);
	}
}

/// Label, topology and detach changes each accompany an otherwise sparse
/// weight.
fn weight_mixed_structural_publication(io_mode: IoMode) {
	for scenario in 0..3 {
		let mut fixture = weight_fixture(io_mode);
		let local = match &fixture.store.inner.backend {
			StoreBackend::Local(local) => Some(local),
			StoreBackend::InMemory => None,
		}
		.assert_some();
		let root = fixture.store.inner.current_root();
		let globals =
			fixture.records.each_ref().map(|record| root.resolve_atom(record).assert_some());
		drop(root);
		let files = local.state.lock().file_catalog.files.clone();
		let mut write = weight_candidate(&fixture);
		match scenario {
			0 => write
				.set_occurrence_label(fixture.graph.id(), &fixture.records[1], Some("F".into()))
				.assert_ok(),
			1 => {
				let mut read = fixture.store.begin();
				let a = read
					.membership(fixture.graph.id(), &"vertex:a".parse().assert_ok())
					.assert_ok()
					.assert_some();
				let b = read
					.membership(fixture.graph.id(), &"vertex:b".parse().assert_ok())
					.assert_ok()
					.assert_some();
				write
					.replace_occurrence_state(
						fixture.graph.id(),
						&fixture.records[1],
						OccurrenceState::edge(
							EdgeTopology::directed([b], [a]),
							Some(2_i64.into()),
							None,
						),
					)
					.assert_ok();
			}
			_ => {
				write.detach_occurrence(fixture.graph.id(), &fixture.records[2], false).assert_ok();
			}
		}
		write.commit().assert_ok();
		let current_files = local.state.lock().file_catalog.files.clone();
		for entry in &files {
			let affected = entry.space_id
				== crate::datastore::DataSpaceId::Graph(fixture.graph.id())
				&& (matches!(
					entry.file_id.kind(),
					Some(
						DataFileKind::Index
							| DataFileKind::Statistics
							| DataFileKind::LabelStatistics
					)
				) || entry.path.as_str().ends_with("/labels.fgb"));
			if !affected {
				assert!(
					current_files.contains(entry),
					"MIXED_STATE_REBUILT_UNRELATED_FILE: {} scenario={scenario}",
					entry.path.as_str()
				);
			}
		}
		assert_weight_mixed_result(&fixture, scenario, &globals);
		drop(fixture.store);
		fixture.store = Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
		assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, io_mode);
		assert_weight_mixed_result(&fixture, scenario, &globals);
	}
}

/// Literal globals, topology and payloads prevent a self-consistent wrong root
/// passing.
fn assert_weight_mixed_result(
	fixture: &WeightFixture,
	scenario: usize,
	globals: &[crate::datastore::GlobalAtomId; 3],
) {
	assert_weight_metadata(&fixture.store, fixture.graph.id());
	let root = fixture.store.inner.current_root();
	let graph = root.graph(fixture.graph.id()).assert_some();
	let membership = |record: &RecordId| {
		root.membership(fixture.graph.id(), root.resolve_atom(record).assert_some()).assert_some()
	};
	let a = membership(&"vertex:a".parse().assert_ok());
	let b = membership(&"vertex:b".parse().assert_ok());
	let mut read = fixture.store.begin();
	let query = read.query_snapshot().assert_ok();
	for (index, (record, global)) in fixture.records.iter().zip(globals).enumerate() {
		assert_eq!(root.resolve_atom(record), Some(*global));
		let base = root.atom(*global).assert_some();
		assert_eq!(base.properties().len(), 1);
		assert_eq!(base.properties().get(fixture.payload), Some(&Value::from("base")));
		if scenario == 2 && index == 2 {
			assert_eq!(root.membership(fixture.graph.id(), *global), None);
			assert!(root.occurrence(fixture.graph.id(), fixture.edges[index]).is_none());
			continue;
		}
		assert_eq!(root.membership(fixture.graph.id(), *global), Some(fixture.edges[index]));
		let occurrence = root.occurrence(fixture.graph.id(), fixture.edges[index]).assert_some();
		assert_eq!(occurrence.global_id(), *global);
		let topology = if scenario == 1 && index == 1 {
			EdgeTopology::directed([b], [a])
		} else {
			fixture.topology.clone()
		};
		let weight = match index {
			0 => Some(1_i64.into()),
			1 => Some(2_i64.into()),
			_ => None,
		};
		assert_eq!(occurrence.state(), &OccurrenceState::edge(topology, weight, None));
		let label = if scenario == 0 && index == 1 {
			"F"
		} else {
			"E"
		};
		assert_eq!(occurrence.label().map(|label| label.as_str()), Some(label));
		if index == 2 {
			assert_eq!(
				occurrence.overlay().assert_some().get(fixture.payload),
				Some(&crate::datastore::OverlayValue::Hidden)
			);
		}
		let fields = read
			.query_physical_fields(&query, &fixture.graph, fixture.edges[index])
			.assert_ok()
			.assert_some();
		assert_eq!(&fields.record_id, record);
		let mut expected = PropertyMap::new();
		match index {
			0 => {
				expected.insert(fixture.payload, Value::from("overlay"));
			}
			1 => {
				expected.insert(fixture.payload, Value::from("base"));
			}
			_ => {}
		}
		assert_eq!(fields.properties, expected);
	}
	let local = match &fixture.store.inner.backend {
		StoreBackend::Local(local) => Some(local),
		StoreBackend::InMemory => None,
	}
	.assert_some();
	assert_physical_weights(
		local,
		fixture.graph.id(),
		&[fixture.edges[0], fixture.edges[1]],
		local.wal.snapshot(),
		[Some(1.0), Some(2.0)],
	);
	if scenario == 2 {
		let expected_generation = fixture.edges[2].generation() + 1;
		assert_eq!(
			graph.slot_generation(
				AtomKind::Edge,
				usize::try_from(fixture.edges[2].slot()).assert_ok()
			),
			Some(expected_generation)
		);
		let state = local.state.lock();
		let files = state
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == fixture.graph.id())
			.assert_some()
			.files;
		let (number, within) = hot_slot_address(fixture.edges[2].slot()).assert_ok();
		let page = local
			.read_update_page(&state, PageKey::new(files.space(), files.edges, number))
			.assert_ok();
		assert_eq!(
			read_edge_hot_slot(&page, number, within).assert_ok(),
			crate::storage::graph_pages::EdgeHotSlot::tombstone(expected_generation)
		);
	} else {
		assert_physical_weights(
			local,
			fixture.graph.id(),
			&[fixture.edges[2]],
			local.wal.snapshot(),
			[None],
		);
	}
	assert_weight_mixed_traversal(fixture, scenario, [a, b]);
	super::label_tests::assert_label_arrays(&fixture.store, fixture.graph.id());
	assert_weight_mixed_index(fixture, scenario, local, &root);
}

/// Canonical scalar postings must preserve HIDDEN and remove detached owners.
fn assert_weight_mixed_index(
	fixture: &WeightFixture,
	scenario: usize,
	local: &LocalStore,
	root: &crate::datastore::DatabaseRoot,
) {
	let class = root.schema.class("link").assert_some();
	let index = class.indexes().find(|index| index.name().as_str() == "effective").assert_some();
	assert_eq!(index.kind(), ClassIndexKind::Standard);
	assert_eq!(index.scope(), IndexScope::Effective {
		graph: fixture.graph.id()
	});
	let index = index.id().assert_some();
	let pin = local.capture_query_snapshot(root.commit_ts).assert_ok().assert_some();
	let mut context = crate::datastore::index_context(&root.schema, &root.dictionary).assert_ok();
	// HIDDEN remains an indexed NONE value until the occurrence is detached.
	for (value, owner) in [
		(Value::from("overlay"), Some(fixture.edges[0])),
		(Value::from("base"), Some(fixture.edges[1])),
		(Value::None, (scenario != 2).then_some(fixture.edges[2])),
	] {
		let key =
			crate::values::storage::encode_index_key(&[(&value, &Type::String)], &mut context)
				.assert_ok();
		let expected =
			owner.into_iter().map(|owner| (AtomKind::Edge, owner.slot())).collect::<Vec<_>>();
		super::assert_overlay_snapshot_postings(local, &pin, index, &key, &expected);
	}
	let decoded = local.physical_index(&pin, index).assert_ok().assert_some();
	let groups = match &decoded.entries {
		crate::storage::PhysicalIndexEntries::Scalar(groups) => Some(groups),
		_ => None,
	}
	.assert_some();
	let expected_count = if scenario == 2 {
		2
	} else {
		3
	};
	assert_eq!(groups.len(), expected_count);
	assert_eq!(groups.iter().map(|group| group.atoms.len()).sum::<usize>(), expected_count);
}

/// Literal arcs independently check physical EDGE topology and NODE adjacency.
fn assert_weight_mixed_traversal(fixture: &WeightFixture, scenario: usize, [a, b]: [AtomId; 2]) {
	let mut read = fixture.store.begin();
	let query = read.query_snapshot().assert_ok();
	let arcs = match scenario {
		0 => vec![(fixture.edges[0], a, b), (fixture.edges[1], a, b), (fixture.edges[2], a, b)],
		1 => vec![(fixture.edges[0], a, b), (fixture.edges[1], b, a), (fixture.edges[2], a, b)],
		_ => vec![(fixture.edges[0], a, b), (fixture.edges[1], a, b)],
	};
	for (source, forward) in [(a, true), (b, true), (a, false), (b, false)] {
		let physical = read
			.query_physical_traversal(&query, &fixture.graph, source, forward, !forward, false)
			.assert_ok()
			.assert_some();
		let expected = arcs
			.iter()
			.filter_map(|&(edge, from, to)| {
				if forward && from == source {
					Some((edge, to))
				} else if !forward && to == source {
					Some((edge, from))
				} else {
					None
				}
			})
			.collect::<Vec<_>>();
		assert_eq!(
			physical
				.candidates
				.iter()
				.map(|candidate| (candidate.edge, candidate.target))
				.collect::<Vec<_>>(),
			expected
		);
	}
}

/// Creates one candidate that changes weights, overlays and label counters.
fn weight_candidate(fixture: &WeightFixture) -> crate::datastore::Transaction {
	let mut write = fixture.store.begin();
	for (record, value) in fixture.records[..2].iter().zip([1_i64, 2]) {
		write
			.replace_occurrence_state(
				fixture.graph.id(),
				record,
				OccurrenceState::edge(fixture.topology.clone(), Some(value.into()), None),
			)
			.assert_ok();
	}
	write
		.set_overlay(fixture.graph.id(), &fixture.records[0], fixture.payload, "overlay".into())
		.assert_ok();
	write.hide_property(fixture.graph.id(), &fixture.records[2], fixture.payload).assert_ok();
	write
}

/// Compares physical state and properties instead of trusting only the RAM
/// root.
fn assert_weight_candidate(fixture: &WeightFixture, published: bool) {
	assert_weight_metadata(&fixture.store, fixture.graph.id());
	let local = match &fixture.store.inner.backend {
		StoreBackend::Local(local) => Some(local),
		StoreBackend::InMemory => None,
	}
	.assert_some();
	let weights = if published {
		[Some(1.0), Some(2.0), None]
	} else {
		[None; 3]
	};
	assert_physical_weights(
		local,
		fixture.graph.id(),
		&fixture.edges,
		local.wal.snapshot(),
		weights,
	);
	assert_overlay_property(
		&fixture.store,
		fixture.graph.id(),
		&fixture.records[0],
		fixture.payload,
		if published {
			"overlay"
		} else {
			"base"
		}
		.into(),
	);
	assert_overlay_label_statistic(
		&fixture.store,
		fixture.graph.id(),
		AtomKind::Edge,
		"E",
		3,
		if published {
			2
		} else {
			3
		},
	);
}

/// Known preparation failures preserve the source; uncertain WAL requires
/// reopen.
fn weight_failures(io_mode: IoMode) {
	use crate::storage::wal::{CommitFailureGuard, CommitFailurePoint};
	{
		let fixture = weight_fixture(io_mode);
		let local = match &fixture.store.inner.backend {
			StoreBackend::Local(local) => Some(local),
			StoreBackend::InMemory => None,
		}
		.assert_some();
		let stamp = fixture.store.inner.current_root().commit_ts;
		let boundary = local.wal.snapshot().visible_lsn();
		let files = local.state.lock().file_catalog.files.clone();
		let header = local.state.lock().graph_superblocks[&fixture.graph.id()];
		for point in [
			GenerationFailurePoint::AfterBuild,
			GenerationFailurePoint::AfterCatalogStage,
			GenerationFailurePoint::BeforeWalCommit,
		] {
			let write = weight_candidate(&fixture);
			{
				let _failure = GenerationFailureGuard::set(point);
				write.commit().assert_err();
			}
			assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
			assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
			assert_eq!(local.state.lock().file_catalog.files, files);
			assert_eq!(local.state.lock().graph_superblocks[&fixture.graph.id()], header);
			assert_weight_candidate(&fixture, false);
		}
		weight_candidate(&fixture).commit().assert_ok();
		assert_eq!(local.state.lock().file_catalog.files, files);
		assert_weight_candidate(&fixture, true);
	}
	for point in [
		CommitFailurePoint::BeforeAppend,
		CommitFailurePoint::AfterAppend,
		CommitFailurePoint::AfterFlush,
		CommitFailurePoint::AfterSync,
	] {
		let mut fixture = weight_fixture(io_mode);
		let stamp = fixture.store.inner.current_root().commit_ts;
		let write = weight_candidate(&fixture);
		{
			let local = match &fixture.store.inner.backend {
				StoreBackend::Local(local) => Some(local),
				StoreBackend::InMemory => None,
			}
			.assert_some();
			let files = local.state.lock().file_catalog.files.clone();
			let header = local.state.lock().graph_superblocks[&fixture.graph.id()];
			{
				let _failure = CommitFailureGuard::set(point);
				write.commit().assert_err();
			}
			assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
			assert_eq!(local.state.lock().file_catalog.files, files);
			assert_eq!(local.state.lock().graph_superblocks[&fixture.graph.id()], header);
			assert_weight_candidate(&fixture, false);
			weight_candidate(&fixture).commit().assert_err();
		}
		drop(fixture.store);
		fixture.store = Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
		assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, io_mode);
		let published = point != CommitFailurePoint::BeforeAppend;
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp + u64::from(published));
		assert_weight_candidate(&fixture, published);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		drop(fixture.store);
		fixture.store = Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
		assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, io_mode);
		assert_weight_candidate(&fixture, published);
	}
}

/// Replays concurrent fields on one edge, then independent edges on one page.
fn weight_replay(io_mode: IoMode) {
	for overlay_first in [false, true] {
		let mut fixture = weight_fixture(io_mode);
		let local = match &fixture.store.inner.backend {
			StoreBackend::Local(local) => Some(local),
			StoreBackend::InMemory => None,
		}
		.assert_some();
		let files = local.state.lock().file_catalog.files.clone();
		let mut weight = fixture.store.begin();
		let mut overlay = fixture.store.begin();
		weight
			.replace_occurrence_state(
				fixture.graph.id(),
				&fixture.records[0],
				OccurrenceState::edge(fixture.topology.clone(), Some(1_i64.into()), None),
			)
			.assert_ok();
		overlay
			.set_overlay(fixture.graph.id(), &fixture.records[0], fixture.payload, "overlay".into())
			.assert_ok();
		if overlay_first {
			overlay.commit().assert_ok();
			weight.commit().assert_ok();
		} else {
			weight.commit().assert_ok();
			overlay.commit().assert_ok();
		}
		assert_physical_weights(local, fixture.graph.id(), &fixture.edges, local.wal.snapshot(), [
			Some(1.0),
			None,
			None,
		]);
		let mut left = fixture.store.begin();
		let mut right = fixture.store.begin();
		left.replace_occurrence_state(
			fixture.graph.id(),
			&fixture.records[0],
			OccurrenceState::edge(fixture.topology.clone(), Some(3_i64.into()), None),
		)
		.assert_ok();
		right
			.replace_occurrence_state(
				fixture.graph.id(),
				&fixture.records[1],
				OccurrenceState::edge(fixture.topology.clone(), Some(2_i64.into()), None),
			)
			.assert_ok();
		right.commit().assert_ok();
		left.commit().assert_ok();
		assert_eq!(local.state.lock().file_catalog.files, files);
		assert_physical_weights(local, fixture.graph.id(), &fixture.edges, local.wal.snapshot(), [
			Some(3.0),
			Some(2.0),
			None,
		]);
		assert_overlay_property(
			&fixture.store,
			fixture.graph.id(),
			&fixture.records[0],
			fixture.payload,
			"overlay".into(),
		);
		drop(fixture.store);
		fixture.store = Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
		assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, io_mode);
		let local = match &fixture.store.inner.backend {
			StoreBackend::Local(local) => Some(local),
			StoreBackend::InMemory => None,
		}
		.assert_some();
		assert_physical_weights(local, fixture.graph.id(), &fixture.edges, local.wal.snapshot(), [
			Some(3.0),
			Some(2.0),
			None,
		]);
		assert_overlay_property(
			&fixture.store,
			fixture.graph.id(),
			&fixture.records[0],
			fixture.payload,
			"overlay".into(),
		);
	}
}

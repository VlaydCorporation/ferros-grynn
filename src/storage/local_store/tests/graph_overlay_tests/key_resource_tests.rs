//! Unchanged inherited keys still require bounded conversion before commit.

use super::*;
use crate::{
	datastore::{DataSpaceId, DatabaseRoot, GlobalAtomId, IndexId, PublicationDelta},
	gql_status::{FerrosGrynnError, Status},
	graph::GraphId,
	memory::tree_retained_bytes,
	storage::{
		atom_store_image::value_context,
		graph_index::with_external_sort_budget,
		local_store::{
			image::{initial_context_reservation, point_value_context},
			point_index::PointIndexPlan,
			point_namespace::{LabelPlan, PublicationBudget},
			point_update::PointUpdatePlan,
			tests::base_resource_tests::{assert_mode, local},
		},
		types::Lsn,
	},
	values::{Array, storage::StorageValue},
};

/// The singleton key fits a full Standard leaf but its DTO is much larger.
const ELEMENTS: usize = 4000;

/// One unchanged array and one independent overlay isolate key preparation.
struct Fixture {
	/// The directory owns cleanup only after all local stores and pins close.
	directory: TestDirectory,
	/// Production defaults remain the positive control.
	defaults: LocalOptions,
	/// Cold-restored resolver ownership is separate from the unchanged payload
	/// cap.
	initial: usize,
	/// Plain NODE membership owner.
	graph: GraphId,
	/// Stable public/global identity.
	record: RecordId,
	/// The indexed field stays inherited on both sides.
	indexed: PropertyKeyId,
	/// Only this preexisting overlay value changes.
	small: PropertyKeyId,
	/// One Ready Standard Effective binding.
	index: IndexId,
	/// Independent expected Base/effective value.
	value: Value,
	/// Legacy byte oracle for the physical posting.
	key: Vec<u8>,
}

/// Sync denial preserves the durable source through a limited cold reopen.
#[test]
fn scalar_key_commit_sync_rejects_dto_before_index_output() {
	key_commit(IoMode::Sync);
}

/// Actual NativeAsync checks the same create/open/reopen mode and admission.
#[cfg(feature = "native-async-io")]
#[test]
fn scalar_key_commit_native_async_rejects_dto_before_index_output() {
	key_commit(IoMode::NativeAsync);
}

impl Fixture {
	/// Derives the ceiling from the exact DTO backing, not an arbitrary cap.
	const fn maximum() -> usize {
		ELEMENTS * size_of::<StorageValue>()
	}

	/// Adds only estimated resolver ownership to the existing payload
	/// allowance.
	fn total_maximum(&self) -> usize {
		self.initial.checked_add(Self::maximum()).assert_some()
	}

	/// The valid singleton leaf leaves room for its directory and inline
	/// posting.
	fn indexed_type() -> Type {
		Type::Array(Box::new(Type::Option(Box::new(Type::Bool))))
	}

	/// Seeds under unchanged defaults and makes all source pages durable.
	fn seed(mode: IoMode) -> Self {
		let directory = TestDirectory::new();
		let mut defaults = LocalOptions {
			io_mode: mode,
			buffer_pool_pages: 32,
			..LocalOptions::default()
		};
		defaults.checkpoint.unbackfilled_page_percent = 100;
		defaults.checkpoint.interval = Duration::from_secs(3600);
		let store = Datastore::create_local(&directory.0, defaults).assert_ok();
		assert_mode(&store, mode);
		store.inner.maintenance.shutdown();
		let mut setup = store.begin();
		let graph = setup.define_graph("g", GraphKind::Graph).assert_ok().id();
		let class = ClassSchema::new("item", AtomKind::Node, [
			ClassField::new("indexed", Self::indexed_type()),
			ClassField::new("small", Type::Int),
		])
		.assert_ok()
		.with_index(
			ClassIndex::new(
				"inherited",
				ClassIndexKind::Standard,
				IndexScope::Effective {
					graph,
				},
				["indexed"],
			)
			.assert_ok(),
		)
		.assert_ok();
		setup.define_class(class).assert_ok();
		let indexed = setup.intern_property("indexed").assert_ok();
		let small = setup.intern_property("small").assert_ok();
		let value = Value::Array(Array::new(vec![Value::None; ELEMENTS]));
		let record = "item:a".parse::<RecordId>().assert_ok();
		let mut properties = PropertyMap::new();
		properties.insert(indexed, value.clone());
		setup.create_atom(record.clone(), properties).assert_ok();
		setup.attach_node(graph, &record, None, None).assert_ok();
		setup.set_overlay(graph, &record, small, 0.into()).assert_ok();
		setup.commit().assert_ok();
		store.checkpoint(CheckpointMode::Restart).assert_ok();
		let root = store.inner.current_root();
		let published_class = root.schema.class("item").assert_some();
		let definition = published_class.indexes().next().assert_some();
		assert_eq!(definition.state(), ClassIndexState::Ready);
		let index = definition.id().assert_some();
		let mut context = value_context(&root.schema, root.dictionary.as_ref().clone()).assert_ok();
		let key = crate::values::storage::encode_index_key(
			&[(&value, &Self::indexed_type())],
			&mut context,
		)
		.assert_ok();
		assert_eq!(key.len(), 4 * ELEMENTS + 3);
		assert!(44 + 2 + key.len() + 8 + 16 < crate::storage::page::PAGE_SIZE);
		drop((root, store));
		let cold = Datastore::open_local(&directory.0, defaults).assert_ok();
		assert_mode(&cold, mode);
		cold.inner.maintenance.shutdown();
		let (initial, overlap) =
			initial_context_reservation(&cold.inner.current_root()).assert_ok();
		assert!(initial > 0);
		assert_eq!(overlap, 0);
		drop(cold);
		Self {
			directory,
			defaults,
			initial,
			graph,
			record,
			indexed,
			small,
			index,
			value,
			key,
		}
	}

	/// Limited and default opens both preserve requested native mode.
	fn open(&self, limited: bool) -> Datastore {
		let mut options = self.defaults;
		if limited {
			options.maintenance.max_memory_bytes =
				NonZeroUsize::new(self.total_maximum()).assert_some();
		}
		let store = Datastore::open_local(&self.directory.0, options).assert_ok();
		assert_mode(&store, self.defaults.io_mode);
		store.inner.maintenance.shutdown();
		assert_eq!(
			initial_context_reservation(&store.inner.current_root()).assert_ok(),
			(self.initial, 0)
		);
		store
	}

	/// Checks owned logical/physical values and the untouched scalar posting.
	fn assert_state(&self, store: &Datastore, small: i64) {
		assert_overlay_property(store, self.graph, &self.record, self.small, small.into());
		assert_overlay_property(store, self.graph, &self.record, self.indexed, self.value.clone());
		let root = store.inner.current_root();
		let global = root.resolve_atom(&self.record).assert_some();
		let owner = root.membership(self.graph, global).assert_some();
		assert!(root.occurrence(self.graph, owner).assert_some().inherits(self.indexed));
		let storage = local(store);
		let pin = storage.capture_query_snapshot(root.commit_ts).assert_ok().assert_some();
		assert_overlay_snapshot_postings(storage, &pin, self.index, &self.key, &[(
			AtomKind::Node,
			owner.slot(),
		)]);
	}
}

/// A typed child-vector rejection is stronger than an arbitrary resource error.
fn assert_dto_rejection(error: &FerrosGrynnError) {
	let expected = ((ELEMENTS + 1) * size_of::<StorageValue>()) as u64;
	assert!(
		matches!(error.status().status(), Status::StorageDecodeLimitExceeded { limit, actual, maximum }
		if limit == "runtime storage encoding" && *actual == expected && *maximum < Fixture::maximum() as u64 && actual > maximum)
	);
}

/// The replay has one overlay and no global/membership/topology mutation.
fn roots(store: &Datastore, fixture: &Fixture) -> (DatabaseRoot, DatabaseRoot, PublicationDelta) {
	let storage = local(store);
	let mut before = store.inner.current_root().as_ref().clone();
	storage.hydrate_statistics(&mut before).assert_ok();
	let global = before.resolve_atom(&fixture.record).assert_some();
	let owner = before.membership(fixture.graph, global).assert_some();
	let mut after = before.clone();
	after.set_overlay(fixture.graph, owner, fixture.small, 1.into()).assert_ok();
	after.commit_ts = before.commit_ts + 1;
	assert_eq!(after.resolve_atom(&fixture.record), Some(global));
	assert_eq!(after.membership(fixture.graph, global), Some(owner));
	for root in [&before, &after] {
		assert!(root.occurrence(fixture.graph, owner).assert_some().inherits(fixture.indexed));
		assert_eq!(
			root.atom(global).assert_some().properties().get(fixture.indexed),
			Some(&fixture.value)
		);
	}
	let delta = PublicationDelta {
		records: BTreeSet::from([fixture.record.clone()]),
		global_slots: BTreeSet::new(),
		overlays: BTreeSet::from([(fixture.graph, global)]),
		incidence: BTreeSet::new(),
		memberships: BTreeSet::new(),
		point_graph_only: true,
	};
	assert!(PointUpdatePlan::eligible(&before, &after, &delta).assert_ok());
	(before, after, delta)
}

/// Proves the same-cap complete physical prefix and exact subsequent failure.
fn assert_prefix(store: &Datastore, fixture: &Fixture) {
	let (before, after, delta) = roots(store, fixture);
	let storage = local(store);
	for (maximum, limited) in [
		(fixture.total_maximum(), true),
		(fixture.defaults.maintenance.max_memory_bytes.get(), false),
	] {
		with_external_sort_budget(maximum, || {
			let state = storage.state.lock();
			let mut budget = PublicationBudget::new();
			let mut context =
				point_value_context(&after, &state._database_catalog.time_zones, &mut budget)?;
			let mut labels =
				LabelPlan::build_with_budget(storage, &state, &before, &after, &delta, budget)?;
			let point = PointUpdatePlan::build(
				storage,
				&state,
				&before,
				&after,
				&delta,
				&mut labels,
				&mut context,
			)?;
			// Exclude target-tree, component and root-DTO bookkeeping
			// rejection.
			let bookkeeping = tree_retained_bytes::<(GraphId, GlobalAtomId)>(1)?
				+ 4096
				+ size_of::<(&Value, &Type)>()
				+ size_of::<StorageValue>();
			labels.budget.scratch(bookkeeping)?;
			let result = PointIndexPlan::build(
				storage,
				&state,
				&before,
				&after,
				&delta,
				Some(&point.graph_arenas),
				&mut labels,
				&mut context,
			);
			if limited {
				assert_dto_rejection(&result.err().assert_some());
			} else {
				assert!(result.assert_ok().is_none());
			}
			drop(point);
			Ok(())
		})
		.assert_ok();
	}
}

/// Successful controls must publish only overlay/graph metadata, no Base/index.
fn assert_overlay_frames(store: &Datastore, fixture: &Fixture, boundary: Lsn) {
	let storage = local(store);
	let state = storage.state.lock();
	let atoms = state.bindings.atoms;
	let index = state.bindings.physical_indexes.get(&fixture.index).assert_some().file_id;
	let base = BTreeSet::from([
		atoms.superblock,
		atoms.atoms_hot,
		atoms.properties,
		atoms.property_dictionary,
		atoms.primary,
	]);
	drop(state);
	let frames = storage
		.wal
		.recovery_frames()
		.into_iter()
		.filter(|frame| frame.frame_lsn() > boundary)
		.collect::<Vec<_>>();
	assert!(!frames.is_empty());
	assert!(
		frames.iter().any(|frame| frame.key().file_id().kind()
			== Some(crate::storage::types::DataFileKind::Overlays))
	);
	assert!(frames.iter().all(|frame| !(frame.key().space_id() == DataSpaceId::Database
		&& base.contains(&frame.key().file_id()))));
	assert!(
		frames.iter().all(|frame| !(frame.key().space_id() == DataSpaceId::Graph(fixture.graph)
			&& frame.key().file_id() == index))
	);
}

/// Denial preserves the durable root, WAL boundary, bindings and old pin.
fn assert_rejected_commit(fixture: &Fixture) {
	let store = fixture.open(true);
	fixture.assert_state(&store, 0);
	assert_prefix(&store, fixture);
	let storage = local(&store);
	let root = store.inner.current_root();
	let pin = storage.capture_query_snapshot(root.commit_ts).assert_ok().assert_some();
	let boundary = storage.wal.snapshot().visible_lsn();
	let files = storage.state.lock().file_catalog.files.clone();
	let bindings = storage.state.lock().bindings.physical_indexes.clone();
	let directories = generation_directory_paths(&fixture.directory.0);
	let mut write = store.begin();
	write.set_overlay(fixture.graph, &fixture.record, fixture.small, 1.into()).assert_ok();
	let outcome = write.commit();
	if outcome.is_ok() {
		assert_overlay_frames(&store, fixture, boundary);
		println!(
			"SCALAR_KEY_COMMIT_UNADMITTED {:?} maximum={}",
			fixture.defaults.io_mode,
			Fixture::maximum()
		);
	}
	assert!(outcome.is_err(), "SCALAR_KEY_COMMIT_BUDGET_NOT_REJECTED");
	assert_dto_rejection(&outcome.assert_err());
	assert!(Arc::ptr_eq(&store.inner.current_root(), &root));
	assert_eq!(storage.wal.snapshot().visible_lsn(), boundary);
	assert_eq!(storage.state.lock().file_catalog.files, files);
	assert_eq!(storage.state.lock().bindings.physical_indexes, bindings);
	assert_eq!(generation_directory_paths(&fixture.directory.0), directories);
	let owner = root
		.membership(fixture.graph, root.resolve_atom(&fixture.record).assert_some())
		.assert_some();
	assert_overlay_snapshot_postings(storage, &pin, fixture.index, &fixture.key, &[(
		AtomKind::Node,
		owner.slot(),
	)]);
	drop((pin, root, store));
	let limited = fixture.open(true);
	fixture.assert_state(&limited, 0);
	drop(limited);
}

/// Defaults accept the identical edit and retain an old snapshot through
/// commit.
fn assert_default_commit(fixture: &Fixture) {
	let normal = fixture.open(false);
	let boundary = local(&normal).wal.snapshot().visible_lsn();
	let mut old = normal.begin();
	assert_eq!(
		old.effective_property(fixture.graph, &fixture.record, fixture.small).assert_ok(),
		Some(0.into())
	);
	let mut write = normal.begin();
	write.set_overlay(fixture.graph, &fixture.record, fixture.small, 1.into()).assert_ok();
	write.commit().assert_ok();
	assert_overlay_frames(&normal, fixture, boundary);
	assert_eq!(
		old.effective_property(fixture.graph, &fixture.record, fixture.small).assert_ok(),
		Some(0.into())
	);
	fixture.assert_state(&normal, 1);
	drop(old);
	normal.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(normal);
	let reopened = fixture.open(false);
	fixture.assert_state(&reopened, 1);
}

/// Both backends run the same denial and durable positive control.
fn key_commit(mode: IoMode) {
	let fixture = Fixture::seed(mode);
	assert_rejected_commit(&fixture);
	assert_default_commit(&fixture);
}

//! Detached Base updates and leaf replacement publish one retained physical
//! slice.

use super::*;
use crate::{
	datastore::{DatabaseRoot, GlobalAtomId, IndexId, PublicationDelta},
	gql_status::FerrosGrynnResult,
	storage::{
		LocalQuerySnapshot,
		PhysicalIndexEntries,
		catalog::FileCatalogEntry,
		local_store::{
			image::point_value_context,
			point_index::PointIndexPlan,
			point_namespace::{LabelPlan, PublicationBudget},
			point_update::PointUpdatePlan,
		},
		page::AlignedPage,
	},
};

/// Every seed is published before candidate identities and pins are captured.
struct Case {
	/// Owns the cold local store and its isolated directory.
	fixture: Fixture,
	/// Existing scalar key cannot change the property dictionary.
	key: PropertyKeyId,
	/// One Base owner has no memberships in any graph.
	record: RecordId,
	/// Full generation-aware Base identity is fixed across publication.
	global: GlobalAtomId,
	/// Existing NODE or EDGE container retains its full local handle.
	selected: AtomId,
	/// Old and replacement leaves are public and nonempty.
	children: [GraphId; 2],
	/// Explicit Base Standard, Unique and Count controls share the commit.
	indexes: [IndexId; 3],
}

/// Small scalar leaves and the containing graph use real production fixtures.
fn case_fixture(mode: IoMode, family: GraphKind, kind: AtomKind) -> Case {
	let (fixture, _) = metagraph::container_fixture(mode, family);
	let (mut fixture, replacement) = metagraph::with_replacement(fixture);
	let base_kind = if fg_runtime::__internal::is_node(&(kind)) {
		AtomKind::Edge
	} else {
		AtomKind::Node
	};
	let mut setup = fixture.store.begin();
	let mut class =
		ClassSchema::new("loose", base_kind, [ClassField::new("mixed_value", Type::Int)])
			.assert_ok();
	for (name, index_kind, field) in [
		("mixed_standard", ClassIndexKind::Standard, Some("mixed_value")),
		("mixed_unique", ClassIndexKind::Unique, Some("mixed_value")),
		("mixed_count", ClassIndexKind::Count, None),
	] {
		class = class
			.with_index(ClassIndex::new(name, index_kind, IndexScope::Base, field).assert_ok())
			.assert_ok();
	}
	setup.define_class(class).assert_ok();
	let key = setup.intern_property("mixed_value").assert_ok();
	for index in 0..32_i64 {
		let mut properties = PropertyMap::new();
		properties.insert(key, Value::from(index));
		setup.create_atom(format!("loose:n{index}").parse().assert_ok(), properties).assert_ok();
	}
	setup.commit().assert_ok();
	// Nonzero source generations make raw posting checks detect lost identity
	// bits.
	let reused_record = "loose:n16".parse::<RecordId>().assert_ok();
	let original_global =
		fixture.store.inner.current_root().resolve_atom(&reused_record).assert_some();
	let mut reuse = fixture.store.begin();
	reuse.delete_atom(&reused_record).assert_ok();
	let mut properties = PropertyMap::new();
	properties.insert(key, Value::from(16_i64));
	reuse.create_atom(reused_record.clone(), properties).assert_ok();
	reuse.commit().assert_ok();
	let reused_global =
		fixture.store.inner.current_root().resolve_atom(&reused_record).assert_some();
	assert_eq!(reused_global.slot(), original_global.slot());
	assert_eq!(reused_global.generation(), original_global.generation() + 1);
	assert_ne!(reused_global.generation(), 0);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	// The first old physical reads happen after advancing a real WAL prefix.
	let mut control = fixture.store.begin();
	control
		.set_occurrence_label(
			fixture.graph.id(),
			&child_presence_tests::record(kind),
			Some("mixed-source".into()),
		)
		.assert_ok();
	control.commit().assert_ok();
	let record = reused_record;
	let root = fixture.store.inner.current_root();
	let global = root.resolve_atom(&record).assert_some();
	assert!(root.memberships(global).is_none_or(|members| members.is_empty()));
	let selected_global = root.resolve_atom(&child_presence_tests::record(kind)).assert_some();
	let selected = root.membership(fixture.graph.id(), selected_global).assert_some();
	let old =
		root.occurrence(fixture.graph.id(), selected).assert_some().child_graph().assert_some();
	assert_ne!(old, replacement);
	let class = root.schema.class("loose").assert_some();
	let indexes = ["mixed_standard", "mixed_unique", "mixed_count"].map(|name| {
		class.indexes().find(|index| index.name().as_str() == name).assert_some().id().assert_some()
	});
	drop(root);
	crate::storage::local_store::tests::base_lifecycle_tests::assert_base_raw_postings(
		&fixture.store,
	);
	Case {
		fixture,
		key,
		record,
		global,
		selected,
		children: [old, replacement],
		indexes,
	}
}

/// Repeated writes coalesce to one Base record and one final incidence key.
fn change(case: &Case, operation: u8) -> Transaction {
	let root = case.fixture.store.inner.current_root();
	let old = root.occurrence(case.fixture.graph.id(), case.selected).assert_some();
	let state = child_presence_tests::with_child(old.state(), Some(case.children[1]));
	let mut write = case.fixture.store.begin();
	write
		.replace_occurrence_state(
			case.fixture.graph.id(),
			&child_presence_tests::record(case.selected.kind().assert_some()),
			state,
		)
		.assert_ok();
	match operation {
		0 => {
			write.set_base_property(&case.record, case.key, Value::from(1000_i64)).assert_ok();
		}
		1 => {
			write.remove_base_property(&case.record, case.key).assert_ok();
		}
		2 => {
			write.set_base_property(&case.record, case.key, Value::from(-7_i64)).assert_ok();
			write.remove_base_property(&case.record, case.key).assert_ok();
			write.set_base_property(&case.record, case.key, Value::from(1000_i64)).assert_ok();
		}
		_ => unreachable!("unknown mixed Base fixture operation"),
	}
	write
}

/// Literal Base values and raw full-generation postings are separate oracles.
fn assert_current(case: &Case, operation: u8) {
	let root = case.fixture.store.inner.current_root();
	assert_eq!(root.resolve_atom(&case.record), Some(case.global));
	assert!(root.memberships(case.global).is_none_or(|members| members.is_empty()));
	let expected = (operation != 1).then(|| Value::from(1000_i64));
	assert_eq!(root.atom(case.global).assert_some().properties().get(case.key), expected.as_ref());
	assert_eq!(
		root.occurrence(case.fixture.graph.id(), case.selected).assert_some().child_graph(),
		Some(case.children[1])
	);
	drop(root);
	assert_bulk_image(&case.fixture);
	crate::storage::local_store::tests::base_lifecycle_tests::assert_base_raw_postings(
		&case.fixture.store,
	);
}

/// Facade addresses omit generation; raw current postings above check it too.
fn assert_postings(case: &Case, pin: &LocalQuerySnapshot, old: bool, operation: u8) {
	let root = case.fixture.store.inner.current_root();
	let storage = local(&case.fixture.store);
	let mut context = crate::datastore::index_context(&root.schema, &root.dictionary).assert_ok();
	for index in case.indexes {
		let decoded = storage.physical_index(pin, index).assert_ok().assert_some();
		if let PhysicalIndexEntries::Count(count) = &decoded.entries {
			assert_eq!(*count, 32);
			continue;
		}
		let PhysicalIndexEntries::Scalar(groups) = &decoded.entries else {
			unreachable!("scalar fixture decoded another kind");
		};
		for value in [16_i64, 1000] {
			let key = crate::values::storage::encode_index_key(
				&[(&Value::from(value), &Type::Int)],
				&mut context,
			)
			.assert_ok();
			let actual = groups
				.iter()
				.filter(|group| group.key == key)
				.flat_map(|group| group.atoms.iter().map(|atom| (atom.kind, atom.slot)))
				.collect::<Vec<_>>();
			let present = if old {
				value == 16
			} else {
				operation != 1 && value == 1000
			};
			let expected = present
				.then_some((case.global.kind().assert_some(), case.global.slot()))
				.into_iter()
				.collect::<Vec<_>>();
			assert_eq!(actual, expected);
		}
	}
}

/// File identity is a set contract, independent of catalog row ordering.
fn assert_files(case: &Case, expected: &[FileCatalogEntry]) {
	let storage = local(&case.fixture.store);
	let state = storage.state.lock();
	assert_eq!(
		state.file_catalog.files.len(),
		expected.len(),
		"MIXED_LEAF_BASE_REBUILT_DATA_FILES"
	);
	assert!(
		expected.iter().all(|entry| state.file_catalog.files.contains(entry)),
		"MIXED_LEAF_BASE_REBUILT_DATA_FILES"
	);
}

/// Previously published Base property pages remain immutable append prefixes.
fn property_prefix(case: &Case) -> Vec<(PageKey, Box<AlignedPage>)> {
	let storage = local(&case.fixture.store);
	let state = storage.state.lock();
	(0..state.atom_superblock.properties_pages)
		.map(|number| {
			let key = PageKey::new(
				DataSpaceId::Database,
				state.bindings.atoms.properties,
				PageNo::new(number),
			);
			(key, storage.read_update_page(&state, key).assert_ok())
		})
		.collect()
}

/// Original bytes and WAL versions remain unchanged through checkpoint/reopen.
fn assert_prefix(case: &Case, prefix: &[(PageKey, Box<AlignedPage>)]) {
	assert!(!prefix.is_empty());
	let storage = local(&case.fixture.store);
	let state = storage.state.lock();
	for (key, page) in prefix {
		assert_eq!(storage.read_update_page(&state, *key).assert_ok().as_bytes(), page.as_bytes());
	}
}

/// A successful fallback is insufficient: every stable physical identity stays.
fn functional(mode: IoMode, family: GraphKind, kind: AtomKind) {
	for operation in 0..3 {
		let mut case = case_fixture(mode, family, kind);
		let storage = local(&case.fixture.store);
		let source = case.fixture.store.inner.current_root();
		let prefix = property_prefix(&case);
		let mut old_read = case.fixture.store.begin();
		let old_query = old_read.query_snapshot().assert_ok();
		let old_physical =
			storage.capture_query_snapshot(source.commit_ts).assert_ok().assert_some();
		let (files, home) = {
			let state = storage.state.lock();
			(state.file_catalog.files.clone(), state.database_superblock.checkpoint_lsn)
		};
		let boundary = storage.wal.snapshot().visible_lsn();
		assert!(boundary > home);
		change(&case, operation).commit().assert_ok();
		assert_current(&case, operation);
		eprintln!(
			"MIXED_LEAF_BASE_LOGICAL_PASS {mode:?} {family:?} {kind:?} operation={operation}"
		);
		assert_files(&case, &files);
		assert_prefix(&case, &prefix);
		let current = storage
			.capture_query_snapshot(case.fixture.store.inner.current_root().commit_ts)
			.assert_ok()
			.assert_some();
		assert!(case.fixture.store.checkpoint(CheckpointMode::Passive).assert_ok());
		let advanced = storage.state.lock().database_superblock.checkpoint_lsn;
		assert!(advanced > home && advanced <= boundary);
		assert_eq!(
			old_read
				.query_physical_inner_graph(&old_query, &case.fixture.graph, case.selected)
				.assert_ok()
				.assert_some()
				.graph,
			Some(case.children[0])
		);
		assert_eq!(
			old_read.atom(&case.record).assert_ok().assert_some().properties().get(case.key),
			Some(&Value::from(16_i64))
		);
		assert_postings(&case, &old_physical, true, operation);
		assert_postings(&case, &current, false, operation);
		drop(current);
		drop(old_physical);
		drop(old_query);
		drop(old_read);
		drop(source);
		case.fixture = reopen(case.fixture);
		assert_current(&case, operation);
		assert_files(&case, &files);
		assert_prefix(&case, &prefix);
		case.fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		case.fixture = reopen(case.fixture);
		assert_current(&case, operation);
		assert_files(&case, &files);
		assert_prefix(&case, &prefix);
	}
}

/// Controls differ only by the explicitly omitted half of the same mutation.
fn publication_roots(
	case: &Case,
	base: bool,
	leaf: bool,
) -> (DatabaseRoot, DatabaseRoot, PublicationDelta) {
	let current = case.fixture.store.inner.current_root();
	let owner = current.occurrence(case.fixture.graph.id(), case.selected).assert_some();
	let state = child_presence_tests::with_child(owner.state(), Some(case.children[1]));
	let (before, mut after, mut delta) = metagraph::leaf_roots(&case.fixture, case.selected, state);
	if !leaf {
		after = before.clone();
		delta.incidence.clear();
	}
	if base {
		after.set_base_property(case.global, case.key, Value::from(1000_i64)).assert_ok();
		delta.records.insert(case.record.clone());
		delta.global_slots.insert(case.global.slot());
	}
	after.commit_ts = before.commit_ts + 1;
	(before, after, delta)
}

/// Builds both plans and returns their expected selection for caller
/// assertions.
///
/// # Errors
/// Propagates admission, source, codec and I/O errors without staging pages.
fn prepare_publication(
	case: &Case,
	roots: (&DatabaseRoot, &DatabaseRoot),
	delta: &PublicationDelta,
) -> FerrosGrynnResult<bool> {
	let store = local(&case.fixture.store);
	let state = store.state.lock();
	let mut budget = PublicationBudget::new();
	let mut context =
		point_value_context(roots.1, &state._database_catalog.time_zones, &mut budget)?;
	let mut labels = LabelPlan::build_with_budget(store, &state, roots.0, roots.1, delta, budget)?;
	let point =
		PointUpdatePlan::build(store, &state, roots.0, roots.1, delta, &mut labels, &mut context)?;
	let indexes = PointIndexPlan::build(
		store,
		&state,
		roots.0,
		roots.1,
		delta,
		Some(&point.graph_arenas),
		&mut labels,
		&mut context,
	)?;
	let selected = point.graph_arenas.contains_key(&case.fixture.graph.id())
		!= delta.incidence.is_empty()
		&& (delta.global_slots.is_empty() || indexes.is_some());
	drop((point, indexes));
	Ok(selected)
}

/// Both the logical pair and independent physical oracles remain accepted.
fn assert_old(case: &Case) {
	let root = case.fixture.store.inner.current_root();
	assert_eq!(root.resolve_atom(&case.record), Some(case.global));
	assert_eq!(
		root.atom(case.global).assert_some().properties().get(case.key),
		Some(&Value::from(16_i64))
	);
	assert_eq!(
		root.occurrence(case.fixture.graph.id(), case.selected).assert_some().child_graph(),
		Some(case.children[0])
	);
	drop(root);
	assert_bulk_image(&case.fixture);
	crate::storage::local_store::tests::base_lifecycle_tests::assert_base_raw_postings(
		&case.fixture.store,
	);
}

/// Runtime and catalog retain one accepted immutable arena owner.
fn source_arena(case: &Case) -> crate::storage::graph_pages::GraphArenaBindings {
	let storage = local(&case.fixture.store);
	let state = storage.state.lock();
	let arena = state
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == case.fixture.graph.id())
		.assert_some()
		.arena
		.clone();
	let catalog = state
		._database_catalog
		.graphs
		.iter()
		.find(|entry| entry.id == case.fixture.graph.id())
		.assert_some();
	assert!(arena.same_owner(&catalog.arena));
	arena
}

/// Failed private preparation cannot replace either accepted arena owner.
fn assert_source_arena(case: &Case, expected: &crate::storage::graph_pages::GraphArenaBindings) {
	let actual = source_arena(case);
	assert!(actual.same_owner(expected));
	assert_eq!(&actual, expected);
}

/// Sync Graph Node preserves the paired Base/child publication.
#[test]
fn mixed_leaf_base_sync_graph_node_retains_files_and_pins() {
	functional(IoMode::Sync, GraphKind::Graph, AtomKind::Node);
}

/// Sync Graph Edge preserves the paired Base/child publication.
#[test]
fn mixed_leaf_base_sync_graph_edge_retains_files_and_pins() {
	functional(IoMode::Sync, GraphKind::Graph, AtomKind::Edge);
}

/// Sync Metagraph Node preserves the paired Base/child publication.
#[test]
fn mixed_leaf_base_sync_metagraph_node_retains_files_and_pins() {
	functional(IoMode::Sync, GraphKind::Metagraph, AtomKind::Node);
}

/// Sync Metagraph Edge preserves the paired Base/child publication.
#[test]
fn mixed_leaf_base_sync_metagraph_edge_retains_files_and_pins() {
	functional(IoMode::Sync, GraphKind::Metagraph, AtomKind::Edge);
}

/// NativeAsync Graph Node preserves the paired Base/child publication.
#[cfg(feature = "native-async-io")]
#[test]
fn mixed_leaf_base_native_async_graph_node_retains_files_and_pins() {
	functional(IoMode::NativeAsync, GraphKind::Graph, AtomKind::Node);
}

/// NativeAsync Graph Edge preserves the paired Base/child publication.
#[cfg(feature = "native-async-io")]
#[test]
fn mixed_leaf_base_native_async_graph_edge_retains_files_and_pins() {
	functional(IoMode::NativeAsync, GraphKind::Graph, AtomKind::Edge);
}

/// NativeAsync Metagraph Node preserves the paired Base/child publication.
#[cfg(feature = "native-async-io")]
#[test]
fn mixed_leaf_base_native_async_metagraph_node_retains_files_and_pins() {
	functional(IoMode::NativeAsync, GraphKind::Metagraph, AtomKind::Node);
}

/// NativeAsync Metagraph Edge preserves the paired Base/child publication.
#[cfg(feature = "native-async-io")]
#[test]
fn mixed_leaf_base_native_async_metagraph_edge_retains_files_and_pins() {
	functional(IoMode::NativeAsync, GraphKind::Metagraph, AtomKind::Edge);
}

/// Mixed Base and graph plans share resource admission before publication.
mod resource_tests;

/// Known and uncertain failures recover one complete Base/child pair.
mod recovery_tests;

/// False mixed source pages or cached owners cannot publish the Base prefix.
mod source_tests;

/// Mixed commits preserve every admitted ancestor scope and cold snapshot.
mod ancestor_tests;

/// Two detached Base edits share one leaf publication.
mod batch_tests;

//! Scalar key admission is isolated from Primary and physical output builders.

use std::{collections::BTreeSet, sync::Arc};

use fg_runtime::__internal::TextInternal as _;

use super::{scalar_keys::ScopedChanges, *};
use crate::{
	datastore::{ClassField, ClassIndex, ClassIndexState, ClassSchema, Datastore},
	gql_status::Status,
	graph::AtomKind,
	storage::{atom_store_image::value_context, graph_index::with_external_sort_budget},
	test_assertions::{AssertOption as _, AssertResult as _},
	values::{PropertyMap, RecordId, Type, Value, storage::encode_index_key},
};

/// Even one required scalar output key must fit the caller's preparation cap.
#[test]
fn scalar_key_preparation_rejects_before_required_output_exceeds_budget() {
	let store = Datastore::in_memory();
	store.inner.maintenance.shutdown();
	let class = ClassSchema::new("item", AtomKind::Node, [ClassField::new("value", Type::String)])
		.assert_ok()
		.with_index(
			ClassIndex::new("by_value", ClassIndexKind::Standard, IndexScope::Base, ["value"])
				.assert_ok(),
		)
		.assert_ok();
	let record = "item:a".parse::<RecordId>().assert_ok();
	let mut setup = store.begin();
	setup.define_class(class).assert_ok();
	let key = setup.intern_property("value").assert_ok();
	let mut properties = PropertyMap::new();
	properties.insert(key, Value::from("a".repeat(4096)));
	setup.create_atom(record.clone(), properties).assert_ok();
	setup.commit().assert_ok();
	let published = store.inner.current_root();
	let mut writer = store.begin();
	let before = writer.query_snapshot().assert_ok().logical_root().clone();
	let global = before.resolve_atom(&record).assert_some();
	writer.set_base_property(&record, key, Value::from("b".repeat(4096))).assert_ok();
	let after = writer.query_snapshot().assert_ok().logical_root().clone();
	assert_eq!(after.resolve_atom(&record).assert_some(), global);
	let old = before.atom(global).assert_some();
	let new = after.atom(global).assert_some();
	assert_eq!(old.class_id(), new.class_id());
	let old_value = old.properties().get(key).assert_some();
	let new_value = new.properties().get(key).assert_some();
	assert_ne!(old_value, new_value);
	let context = || value_context(&after.schema, after.dictionary.as_ref().clone()).assert_ok();
	let expected_before =
		encode_index_key(&[(old_value, &Type::String)], &mut context()).assert_ok();
	let expected_after =
		encode_index_key(&[(new_value, &Type::String)], &mut context()).assert_ok();
	assert_ne!(expected_before, expected_after);
	let index =
		before.schema.class("item").assert_some().indexes().next().assert_some().id().assert_some();
	assert_binding(&before, index, global, &expected_before);
	assert_binding(&after, index, global, &expected_after);
	let delta = PublicationDelta {
		records: BTreeSet::from([record.clone()]),
		global_slots: BTreeSet::from([global.slot()]),
		overlays: BTreeSet::new(),
		incidence: BTreeSet::new(),
		memberships: BTreeSet::new(),
		point_graph_only: true,
	};
	let mut high_context = context();
	let control =
		with_external_sort_budget(crate::storage::graph_index::external_sort_run_budget(), || {
			scalar_changes(
				&before,
				&after,
				&delta,
				&mut high_context,
				&mut super::super::point_namespace::PublicationBudget::new(),
			)
		})
		.assert_ok();
	assert_change(&control, index, global, &expected_before, &expected_after);
	assert_pair_overlap(&before, &after, global, key);
	drop(control);
	assert!(expected_before.len() > 1);
	let maximum = expected_before.len() - 1;
	let mut low_context = context();
	let result = with_external_sort_budget(maximum, || {
		scalar_changes(
			&before,
			&after,
			&delta,
			&mut low_context,
			&mut super::super::point_namespace::PublicationBudget::new(),
		)
	});
	if let Ok(changes) = &result {
		assert_change(changes, index, global, &expected_before, &expected_after);
		println!(
			"SCALAR_KEY_PREPARATION_UNADMITTED maximum={maximum} required_key={}",
			expected_before.len()
		);
	}
	assert!(result.is_err(), "SCALAR_KEY_PREPARATION_BUDGET_NOT_ENFORCED");
	let error = result.err().assert_some();
	assert!(matches!(error.status().status(), Status::StorageDecodeLimitExceeded { .. }));
	assert!(Arc::ptr_eq(&store.inner.current_root(), &published));
	assert_eq!(published.atom(global).assert_some().properties().get(key), Some(old_value));
}

/// Both roots must expose one Ready, materialized Standard binding and no
/// graph.
fn assert_binding(root: &DatabaseRoot, index: IndexId, global: GlobalAtomId, expected: &[u8]) {
	let class = root.schema.class("item").assert_some();
	assert_eq!(class.indexes().count(), 1);
	let definition = class.indexes().next().assert_some();
	assert_eq!(definition.id(), Some(index));
	assert_eq!(definition.state(), ClassIndexState::Ready);
	assert_eq!(root.indexes.base.len(), 1);
	assert!(root.indexes.graphs.is_empty());
	assert!(root.memberships(global).is_none_or(|members| members.is_empty()));
	let binding = root.indexes.base.get(&index).assert_some();
	assert_eq!(binding.kind, ClassIndexKind::Standard);
	assert!(!binding.deferred);
	let IndexEntries::Standard(entries) = &binding.entries else {
		unreachable!("Standard scalar fixture exposes another entry kind");
	};
	assert_eq!(entries.len(), 1);
	assert_eq!(entries.get(expected), Some(&BTreeSet::from([global])));
}

/// The expected net change owns both legacy order keys and the full global ID.
fn assert_change(
	changes: &ScopedChanges,
	index: IndexId,
	global: GlobalAtomId,
	before: &[u8],
	after: &[u8],
) {
	assert_eq!(changes.len(), 1);
	let base = changes.get(&None).assert_some();
	assert_eq!(base.len(), 1);
	let prepared = base.get(&index).assert_some();
	assert_eq!(prepared.len(), 1);
	let change = &prepared[0];
	assert_eq!(change.graph, None);
	assert_eq!(change.index, index);
	assert!(matches!(change.address, IndexAddress::Global(atom) if atom == global));
	assert_eq!(change.before.as_deref(), Some(before));
	assert_eq!(change.after.as_deref(), Some(after));
}

/// One independently admitted key cannot fund two overlapping encodings.
fn assert_pair_overlap(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	global: GlobalAtomId,
	key: crate::values::PropertyKeyId,
) {
	let old = before.atom(global).assert_some();
	let new = after.atom(global).assert_some();
	let value = old.properties().get(key).assert_some();
	let context = || value_context(&after.schema, after.dictionary.as_ref().clone()).assert_ok();
	let expected = encode_index_key(&[(value, &Type::String)], &mut context()).assert_ok();
	let single = size_of::<crate::values::storage::StorageValue>()
		+ crate::values::Text::estimated_copy_heap_bytes(4096)
		+ expected.len();
	let (encoded, growth) = crate::values::storage::StorageValue::encode_index_key_bounded(
		&[(value, &Type::String)],
		&mut context(),
		single,
	)
	.assert_ok();
	assert_eq!(encoded, expected);
	assert_eq!(growth, 0);
	let maximum = single + size_of::<(&Value, &Type)>();
	let error =
		after.indexes.prepare_base_replace_bounded(old, new, &mut context(), maximum).assert_err();
	assert!(matches!(error.status().status(), Status::StorageDecodeLimitExceeded { .. }));
	let (changes, persistent) =
		after.indexes.prepare_base_replace_bounded(old, new, &mut context(), 1 << 20).assert_ok();
	assert_eq!(changes.len(), 1);
	let bytes = changes
		.iter()
		.map(|change| {
			change.before.as_ref().map_or(0, Vec::capacity)
				+ change.after.as_ref().map_or(0, Vec::capacity)
		})
		.sum::<usize>();
	assert_eq!(persistent, bytes);
}

/// Equal pairs release only their key copies before visiting the next binding.
#[test]
fn scalar_unchanged_keys_release_preparation_copies_between_bindings() {
	let store = Datastore::in_memory();
	store.inner.maintenance.shutdown();
	let mut class =
		ClassSchema::new("item", AtomKind::Node, [ClassField::new("value", Type::String)])
			.assert_ok();
	for (name, kind) in [("standard", ClassIndexKind::Standard), ("unique", ClassIndexKind::Unique)]
	{
		class = class
			.with_index(ClassIndex::new(name, kind, IndexScope::Base, ["value"]).assert_ok())
			.assert_ok();
	}
	let record = "item:a".parse::<RecordId>().assert_ok();
	let mut setup = store.begin();
	setup.define_class(class).assert_ok();
	let key = setup.intern_property("value").assert_ok();
	let mut properties = PropertyMap::new();
	properties.insert(key, Value::from("a".repeat(4096)));
	setup.create_atom(record.clone(), properties).assert_ok();
	setup.commit().assert_ok();
	let root = store.inner.current_root();
	let global = root.resolve_atom(&record).assert_some();
	let atom = root.atom(global).assert_some();
	assert_eq!(root.indexes.base.len(), 2);
	let mut context = value_context(&root.schema, root.dictionary.as_ref().clone()).assert_ok();
	let expected = encode_index_key(
		&[(atom.properties().get(key).assert_some(), &Type::String)],
		&mut context,
	)
	.assert_ok();
	let maximum = size_of::<(&Value, &Type)>()
		+ size_of::<crate::values::storage::StorageValue>()
		+ crate::values::Text::estimated_copy_heap_bytes(4096)
		+ 2 * expected.len();
	let (changes, persistent) =
		root.indexes.prepare_base_replace_bounded(atom, atom, &mut context, maximum).assert_ok();
	assert!(changes.is_empty());
	assert_eq!(changes.capacity(), 0);
	assert_eq!(persistent, 0);
	assert!(Arc::ptr_eq(&store.inner.current_root(), &root));
}

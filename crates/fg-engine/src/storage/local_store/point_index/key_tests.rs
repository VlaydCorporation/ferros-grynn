//! Primary key preparation must share retained output with sequential encoding.

use std::{collections::BTreeSet, sync::Arc};

use fg_runtime::__internal::TextInternal as _;

use super::*;
use crate::{
	datastore::{ClassSchema, Datastore},
	gql_status::Status,
	graph::AtomKind,
	storage::{atom_store_image::value_context, graph_index::with_external_sort_budget},
	test_assertions::{AssertOption as _, AssertResult as _},
	values::{
		Array,
		Object,
		PropertyMap,
		RecordId,
		RecordIdPart,
		Text,
		Ulid,
		Value,
		storage::{StorageValue, codec::CanonicalCodec},
	},
};

/// A first retained key must reduce the second canonical encoder's envelope.
#[test]
fn primary_key_pair_budget_rejects_retained_key_and_second_encoding_peak() {
	let store = Datastore::in_memory();
	store.inner.maintenance.shutdown();
	let text = Text::from("x".repeat(4096));
	assert_ne!(text.estimated_clone_heap_bytes(), 0);
	let record = RecordId::new("item", RecordIdPart::String(text.clone())).assert_ok();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	let empty = setup.query_snapshot().assert_ok().logical_root().clone();
	setup.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	setup.commit().assert_ok();
	let published = store.inner.current_root();
	let mut writer = store.begin();
	let before = writer.query_snapshot().assert_ok().logical_root().clone();
	let old = before.resolve_atom(&record).assert_some();
	writer.delete_atom(&record).assert_ok();
	writer.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	let after = writer.query_snapshot().assert_ok().logical_root().clone();
	let new = after.resolve_atom(&record).assert_some();
	assert_eq!(new.slot(), old.slot());
	assert_eq!(new.generation(), old.generation() + 1);
	assert_eq!(before.atoms.atom_by_slot(old.slot()).assert_ok().assert_some().id(), old);
	assert_eq!(after.atoms.atom_by_slot(new.slot()).assert_ok().assert_some().id(), new);
	assert!(before.indexes.base.is_empty() && after.indexes.base.is_empty());
	assert!(before.indexes.graphs.is_empty() && after.indexes.graphs.is_empty());
	let delta = PublicationDelta {
		records: BTreeSet::from([record.clone()]),
		global_slots: BTreeSet::from([old.slot()]),
		overlays: BTreeSet::new(),
		incidence: BTreeSet::new(),
		memberships: BTreeSet::new(),
		point_graph_only: true,
	};
	let context = || value_context(&after.schema, after.dictionary.as_ref().clone()).assert_ok();
	let (legacy, maximum, single_peak, retained) = encoding_boundary(&record, &text, &after);
	let candidate = |source: &DatabaseRoot, target: &DatabaseRoot, maximum, prefix| {
		with_external_sort_budget(maximum, || {
			let mut budget = super::super::point_namespace::PublicationBudget::new();
			budget.retain(prefix)?;
			primary_changes(source, target, &delta, &mut context(), &mut budget)
		})
	};
	assert!(candidate(&before, &before, 1, 0).assert_ok().is_empty());
	let single = candidate(&empty, &before, maximum, 0).assert_ok();
	assert_eq!(single.len(), 1);
	assert_eq!(single[0].key, legacy);
	assert_eq!(single[0].atom, old);
	assert!(single[0].insert);
	drop(single);
	let control =
		candidate(&before, &after, crate::storage::graph_index::external_sort_run_budget(), 0)
			.assert_ok();
	assert_pair(&control, &legacy, old, new);
	drop(control);
	let result = candidate(&before, &after, maximum, 0);
	if let Ok(mutations) = &result {
		assert_pair(mutations, &legacy, old, new);
		println!(
			"PRIMARY_KEY_PAIR_UNADMITTED maximum={maximum} single_peak={single_peak} \
			 retained={retained}"
		);
	}
	assert!(result.is_err(), "PRIMARY_KEY_PAIR_BUDGET_NOT_ENFORCED");
	let error = result.err().assert_some();
	assert!(matches!(error.status().status(), Status::StorageDecodeLimitExceeded { .. }));
	// A caller's existing retained prefix must survive entry into this
	// collector.
	let error = candidate(&empty, &before, maximum, maximum - single_peak / 2).err().assert_some();
	assert!(matches!(error.status().status(), Status::StorageDecodeLimitExceeded { .. }));
	assert!(Arc::ptr_eq(&store.inner.current_root(), &published));
	assert_eq!(published.resolve_atom(&record).assert_some(), old);
}

/// Independent canonical controls isolate retained overlap from a bad fixture.
fn encoding_boundary(
	record: &RecordId,
	text: &Text,
	root: &DatabaseRoot,
) -> (Vec<u8>, usize, usize, usize) {
	let context = || value_context(&root.schema, root.dictionary.as_ref().clone()).assert_ok();
	let legacy = encode_record_id(record, &mut context()).assert_ok();
	let dto = StorageValue::from_runtime(Value::from(record.clone()), &mut context()).assert_ok();
	let layout = CanonicalCodec::encoding_layout(dto.as_ref()).assert_ok();
	assert_eq!(layout.scratch_bytes(), 0);
	assert_eq!(layout.encoded_len(), legacy.len());
	let single_peak =
		Text::estimated_copy_heap_bytes(text.len()).checked_add(layout.encoded_len()).assert_some();
	drop(dto);
	let (probe, growth) =
		StorageValue::encode_record_bounded(record, &mut context(), single_peak).assert_ok();
	assert_eq!(probe, legacy);
	assert_eq!(growth, 0);
	let retained = probe.capacity();
	assert_ne!(retained, 0);
	drop(probe);
	// One encoding fits, but its retained predecessor overlaps the next
	// DTO/output.
	let maximum = single_peak.checked_add(retained / 2).assert_some();
	assert!(single_peak <= maximum && maximum < single_peak + retained);
	let (probe, growth) =
		StorageValue::encode_record_bounded(record, &mut context(), maximum).assert_ok();
	assert_eq!(probe, legacy);
	assert_eq!(growth, 0);
	drop(probe);
	let reduced = maximum.checked_sub(retained).assert_some();
	assert!(reduced < single_peak);
	let error = StorageValue::encode_record_bounded(record, &mut context(), reduced).assert_err();
	assert!(matches!(error.status().status(), Status::StorageDecodeLimitExceeded { .. }));
	(legacy, maximum, single_peak, retained)
}

/// Both admission probes require the same complete generation replacement.
fn assert_pair(
	mutations: &[PrimaryIndexMutation<GlobalAtomId>],
	key: &[u8],
	old: GlobalAtomId,
	new: GlobalAtomId,
) {
	assert_eq!(mutations.len(), 2);
	assert_eq!(mutations[0].key.as_slice(), key);
	assert_eq!(mutations[1].key.as_slice(), key);
	assert_eq!(mutations[0].atom, old);
	assert_eq!(mutations[1].atom, new);
	assert!(!mutations[0].insert && mutations[1].insert);
}

/// Every RecordId family keeps canonical bytes while the mutation vector grows.
#[test]
fn primary_key_collection_preserves_scalar_composite_bytes_and_generations() {
	let store = Datastore::in_memory();
	store.inner.maintenance.shutdown();
	let parts = [
		RecordIdPart::Int(7),
		RecordIdPart::String("owned-key".to_owned().into()),
		RecordIdPart::Uuid("00000000-0000-0000-0000-000000000001".parse().assert_ok()),
		RecordIdPart::Ulid("00000000000000000000000001".parse::<Ulid>().assert_ok()),
		RecordIdPart::Array(Array::new(vec![1_i64.into(), "nested".into()])),
		RecordIdPart::Object(Object::from_iter([("nested", Value::from("value"))])),
	];
	let records = parts.map(|part| RecordId::new("item", part).assert_ok());
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	for record in &records {
		setup.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	setup.commit().assert_ok();
	let published = store.inner.current_root();
	let mut writer = store.begin();
	let before = writer.query_snapshot().assert_ok().logical_root().clone();
	for record in &records {
		writer.delete_atom(record).assert_ok();
		writer.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let after = writer.query_snapshot().assert_ok().logical_root().clone();
	let mut expected = BTreeSet::new();
	let mut slots = BTreeSet::new();
	for record in &records {
		let old = before.resolve_atom(record).assert_some();
		let new = after.resolve_atom(record).assert_some();
		assert_eq!(old.slot(), new.slot());
		assert_eq!(new.generation(), old.generation() + 1);
		slots.insert(old.slot());
		expected.insert((false, old));
		expected.insert((true, new));
	}
	let delta = PublicationDelta {
		records: records.into_iter().collect(),
		global_slots: slots,
		overlays: BTreeSet::new(),
		incidence: BTreeSet::new(),
		memberships: BTreeSet::new(),
		point_graph_only: true,
	};
	let mut context = value_context(&after.schema, after.dictionary.as_ref().clone()).assert_ok();
	let mut budget = super::super::point_namespace::PublicationBudget::new();
	let mutations = primary_changes(&before, &after, &delta, &mut context, &mut budget).assert_ok();
	assert_eq!(mutations.len(), expected.len());
	assert_eq!(
		mutations.iter().map(|mutation| (mutation.insert, mutation.atom)).collect::<BTreeSet<_>>(),
		expected
	);
	for mutation in mutations {
		let root = if mutation.insert {
			&after
		} else {
			&before
		};
		let atom = root.atom(mutation.atom).assert_some();
		assert_eq!(mutation.key, encode_record_id(atom.record_id(), &mut context).assert_ok());
	}
	assert!(Arc::ptr_eq(&store.inner.current_root(), &published));
}

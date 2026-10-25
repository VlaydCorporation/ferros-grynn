use std::sync::Arc;

#[cfg(test)]
use crate::test_assertions::AssertOption as _;
use crate::{
	datastore::{
		ClassField,
		ClassIndex,
		ClassIndexKind,
		ClassSchema,
		Datastore,
		IndexScope,
		IndexStoreRoot,
	},
	gql_status::FerrosGrynnResult,
	graph::{AtomKind, GraphKind},
	values::{PropertyMap, RecordId, RecordIdPart, Type, Value},
};

/// Returns record for this module.
fn record(name: &str) -> FerrosGrynnResult<RecordId> {
	RecordId::new("person", RecordIdPart::String(name.into()))
}

/// Compares derived labels to source occurrences, including kind and scope.
fn assert_label_lookup_matches_occurrences(root: &crate::datastore::DatabaseRoot) {
	for (graph, occurrences) in &root.graphs {
		assert!(root.indexes.graphs[graph].labels.values().all(|members| !members.is_empty()));
		for kind in [AtomKind::Node, AtomKind::Edge] {
			for label in ["A", "B", "C"] {
				let expected = occurrences
					.occurrences()
					.filter(|occurrence| {
						occurrence.local_id().kind() == Some(kind)
							&& occurrence.label().is_some_and(|value| value.as_str() == label)
					})
					.map(crate::datastore::GraphOccurrence::local_id)
					.collect::<Vec<_>>();
				assert_eq!(root.indexes.graph_label(*graph, kind, &label.into()), expected);
			}
		}
	}
}

/// Label-only changes must update lookup even without explicit indexes.
#[test]
fn occurrence_label_lookup_tracks_replacements_removals_and_old_roots() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("relation", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = seed.define_graph("selected", GraphKind::Graph).assert_ok();
	let other = seed.define_graph("other", GraphKind::Graph).assert_ok();
	let records = [
		record("a").assert_ok(),
		record("b").assert_ok(),
		"relation:r".parse::<RecordId>().assert_ok(),
	];
	for record in &records {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let a = seed.attach_node(graph.id(), &records[0], Some("A".into()), None).assert_ok();
	let b = seed.attach_node(graph.id(), &records[1], Some("B".into()), None).assert_ok();
	let edge = seed
		.attach_edge(
			graph.id(),
			&records[2],
			Some("A".into()),
			crate::graph::EdgeTopology::directed([a], [b]),
			None,
			None,
		)
		.assert_ok();
	seed.attach_node(other.id(), &records[0], Some("A".into()), None).assert_ok();
	seed.commit().assert_ok();
	let old = store.inner.current_root();
	let mut candidate = old.as_ref().clone();
	assert_label_lookup_matches_occurrences(&candidate);
	for label in [Some("B"), None, Some("C"), Some("C")] {
		for local in [a, edge] {
			candidate.set_occurrence_label(graph.id(), local, label.map(Into::into)).assert_ok();
		}
		assert_label_lookup_matches_occurrences(&candidate);
		assert_label_lookup_matches_occurrences(&old);
		for local in [a, edge] {
			assert_eq!(
				old.occurrence(graph.id(), local).assert_some().label().map(|label| label.as_str()),
				Some("A"),
			);
		}
	}
	for record in [&records[2], &records[0]] {
		let global = candidate.atom_by_record(record).assert_some().id();
		candidate.detach(graph.id(), global).assert_ok().assert_some();
	}
	assert_label_lookup_matches_occurrences(&candidate);
	assert_label_lookup_matches_occurrences(&old);
}

/// A malformed source lookup must fail before inserting a replacement label.
#[test]
fn occurrence_label_lookup_rejects_missing_source_postings() {
	use crate::test_assertions::AssertResult as _;

	let mut indexes = super::model::GraphIndexRoot::default();
	let local = crate::graph::AtomId::from_parts(AtomKind::Node, 4, 0).assert_ok();
	let missing = crate::graph::AtomId::from_parts(AtomKind::Node, 5, 0).assert_ok();
	let before = "A".into();
	let after = "B".into();
	assert!(indexes.replace_label(local, Some(&before), Some(&after)).is_err());
	assert!(indexes.labels.is_empty());
	indexes.replace_label(local, None, Some(&before)).assert_ok();
	let original = indexes.labels.clone();
	assert!(indexes.replace_label(missing, Some(&before), Some(&after)).is_err());
	assert_eq!(indexes.labels, original);
}

/// Verifies base and effective indexes track normalized mutations.
#[test]
fn base_and_effective_indexes_track_normalized_mutations() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut define = store.begin();
	let graph = define.define_graph("work", GraphKind::Graph).assert_ok();
	let class = ClassSchema::new("person", AtomKind::Node, [
		ClassField::new("email", Type::String),
		ClassField::new("age", Type::Int),
	])
	.assert_ok()
	.with_index(
		ClassIndex::new("by_age", ClassIndexKind::Standard, IndexScope::Base, ["age"]).assert_ok(),
	)
	.assert_ok()
	.with_index(
		ClassIndex::new("unique_email", ClassIndexKind::Unique, IndexScope::Base, ["email"])
			.assert_ok(),
	)
	.assert_ok()
	.with_index(
		ClassIndex::new(
			"base_count",
			ClassIndexKind::Count,
			IndexScope::Base,
			std::iter::empty::<&str>(),
		)
		.assert_ok(),
	)
	.assert_ok()
	.with_index(
		ClassIndex::new(
			"effective_email",
			ClassIndexKind::Unique,
			IndexScope::Effective {
				graph: graph.id(),
			},
			["email"],
		)
		.assert_ok(),
	)
	.assert_ok()
	.with_index(
		ClassIndex::new(
			"effective_count",
			ClassIndexKind::Count,
			IndexScope::Effective {
				graph: graph.id(),
			},
			std::iter::empty::<&str>(),
		)
		.assert_ok(),
	)
	.assert_ok();
	define.define_class(class).assert_ok();
	define.commit().assert_ok();

	let schema = store
		.class("person")
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::class_does_not_exist("person"))
		.assert_ok();
	let email = schema
		.fields()
		.find(|field| field.name().as_str() == "email")
		.and_then(|field| field.key)
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("email key absent"))
		.assert_ok();
	let age = schema
		.fields()
		.find(|field| field.name().as_str() == "age")
		.and_then(|field| field.key)
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("age key absent"))
		.assert_ok();

	let first = record("first").assert_ok();
	let second = record("second").assert_ok();
	let mut mutate = store.begin();
	let mut first_values = PropertyMap::new();
	first_values.insert(email, "a@example.test");
	first_values.insert(age, 30_i64);
	mutate.create_atom(first.clone(), first_values).assert_ok();
	let mut second_values = PropertyMap::new();
	second_values.insert(email, "b@example.test");
	second_values.insert(age, 30_i64);
	mutate.create_atom(second.clone(), second_values).assert_ok();
	mutate.attach_node(graph.id(), &first, Some("Engineer".into()), None).assert_ok();
	mutate.attach_node(graph.id(), &second, Some("Engineer".into()), None).assert_ok();
	mutate.commit().assert_ok();

	let root = store.inner.current_root();
	let class_id = schema
		.id()
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument("published class has no id")
		})
		.assert_ok();
	assert_eq!(root.indexes.base_class(class_id).len(), 2);
	assert_eq!(root.indexes.graph_class(graph.id(), class_id).len(), 2);
	assert_eq!(root.indexes.graph_label(graph.id(), AtomKind::Node, &"Engineer".into()).len(), 2);
	let base_count = schema
		.indexes()
		.find(|index| index.name().as_str() == "base_count")
		.and_then(|index| index.id())
		.and_then(|id| root.indexes.base_index(id))
		.and_then(|index| index.entries.count());
	assert_eq!(base_count, Some(2));
	let effective_count = schema
		.indexes()
		.find(|index| index.name().as_str() == "effective_count")
		.and_then(|index| index.id())
		.and_then(|id| root.indexes.graph_index(graph.id(), id))
		.and_then(|index| index.entries.count());
	assert_eq!(effective_count, Some(2));
	drop(root);

	let mut overlay = store.begin();
	overlay.set_overlay(graph.id(), &second, email, Value::from("shared@example.test")).assert_ok();
	overlay.commit().assert_ok();
	let mut conflicting = store.begin();
	let error =
		match conflicting.set_base_property(&first, email, Value::from("shared@example.test")) {
			Ok(_previous) => {
				return {
					let test_result: FerrosGrynnResult<()> =
						Err(crate::gql_status::FerrosGrynnError::invalid_argument(
							"effective unique conflict was not detected",
						));
					test_result.assert_ok();
				};
			}
			Err(error) => error,
		};
	assert_eq!(error.status().code().to_string(), "FG-G2F04");
	assert_eq!(
		conflicting.atom(&first).assert_ok().and_then(|atom| atom.properties().get(email).cloned()),
		Some(Value::from("a@example.test"))
	);
}

/// Verifies reopen rebuild restores effective overlay keys and zero class
/// counts.
#[test]
fn reopen_rebuild_restores_effective_overlay_keys_and_zero_class_counts() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut define = store.begin();
	let graph = define.define_graph("social", GraphKind::Graph).assert_ok();
	let class =
		ClassSchema::new("person", AtomKind::Node, [ClassField::new("email", Type::String)])
			.assert_ok()
			.with_index(
				ClassIndex::new(
					"effective_email",
					ClassIndexKind::Unique,
					IndexScope::Effective {
						graph: graph.id(),
					},
					["email"],
				)
				.assert_ok(),
			)
			.assert_ok();
	define.define_class(class).assert_ok();
	define.commit().assert_ok();

	let schema = store
		.class("person")
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::class_does_not_exist("person"))
		.assert_ok();
	let email = schema
		.fields()
		.find(|field| field.name().as_str() == "email")
		.and_then(|field| field.key)
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("email key absent"))
		.assert_ok();
	let first = record("first").assert_ok();
	let second = record("second").assert_ok();
	let mut write = store.begin();
	let mut first_values = PropertyMap::new();
	first_values.insert(email, "base-a@example.test");
	write.create_atom(first.clone(), first_values).assert_ok();
	let mut second_values = PropertyMap::new();
	second_values.insert(email, "base-b@example.test");
	write.create_atom(second.clone(), second_values).assert_ok();
	write.attach_node(graph.id(), &first, None, None).assert_ok();
	write.attach_node(graph.id(), &second, None, None).assert_ok();
	write.set_overlay(graph.id(), &first, email, Value::from("effective@example.test")).assert_ok();
	write.commit().assert_ok();

	let mut reopened = (*store.inner.current_root()).clone();
	reopened.indexes = Arc::new(IndexStoreRoot::default());
	reopened.statistics = Arc::new(crate::stats::StatisticsStore::default());
	reopened.rebuild_derived_state().assert_ok();
	let class_id = schema
		.id()
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument("published class has no id")
		})
		.assert_ok();
	assert_eq!(reopened.indexes.graph_class(graph.id(), class_id).len(), 2);
	assert_eq!(reopened.statistics.graph(graph.id()).assert_some().classes[&class_id], 2);

	let reopened =
		Datastore::from_root_for_test(crate::datastore::InMemoryOptions::default(), reopened);
	let mut conflict = reopened.begin();
	assert!(
		conflict
			.set_overlay(graph.id(), &second, email, Value::from("effective@example.test"))
			.is_err()
	);
}

/// Creates the same normalized records with optional immediate scalar indexes.
fn replacement_fixture(
	indexed: bool,
) -> FerrosGrynnResult<(Datastore, crate::graph::GraphId, [crate::values::PropertyKeyId; 4])> {
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let graph = tx.define_graph("scope", GraphKind::Metagraph)?;
	let keys = [
		tx.intern_property("age")?,
		tx.intern_property("email")?,
		tx.intern_property("nick")?,
		tx.intern_property("note")?,
	];
	let mut class = ClassSchema::new("person", AtomKind::Node, [
		ClassField::new("age", Type::Int),
		ClassField::new("email", Type::String),
		ClassField::new("nick", Type::String),
		ClassField::new("note", Type::String),
	])?;
	if indexed {
		class = class
			.with_index(ClassIndex::new("age_idx", ClassIndexKind::Standard, IndexScope::Base, [
				"age",
			])?)?
			.with_index(ClassIndex::new(
				"email_unique",
				ClassIndexKind::Unique,
				IndexScope::Base,
				["email"],
			)?)?
			.with_index(ClassIndex::new(
				"nick_unique",
				ClassIndexKind::Unique,
				IndexScope::Effective {
					graph: graph.id(),
				},
				["nick"],
			)?)?
			.with_index(ClassIndex::new(
				"count",
				ClassIndexKind::Count,
				IndexScope::Base,
				std::iter::empty::<&str>(),
			)?)?;
	}
	tx.define_class(class)?;
	for (name, age, email, nick) in
		[("first", 30_i64, "email-a", "nick-a"), ("second", 40_i64, "email-b", "nick-b")]
	{
		let id = record(name)?;
		tx.create_atom(
			id.clone(),
			PropertyMap::from_iter([
				(keys[0], age.into()),
				(keys[1], email.into()),
				(keys[2], nick.into()),
				(keys[3], "note".into()),
			]),
		)?;
		tx.attach_node(graph.id(), &id, Some("Member".into()), None)?;
	}
	tx.commit()?;
	Ok((store, graph.id(), keys))
}

/// Empty index work preserves lookup ownership without skipping data or
/// statistics.
#[test]
fn empty_replacement_keeps_lookup_arc_for_base_and_overlay_changes() {
	use crate::test_assertions::AssertResult as _;

	let (store, graph, [age, _email, _nick, note]) = replacement_fixture(false).assert_ok();
	let first = record("first").assert_ok();
	let before = store.inner.current_root();
	let mut tx = store.begin();
	tx.set_base_property(&first, age, 31.into()).assert_ok();
	tx.commit().assert_ok();
	let after = store.inner.current_root();
	assert!(Arc::ptr_eq(&before.indexes, &after.indexes));
	let atom = after
		.atom_by_record(&first)
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("test atom absent"))
		.assert_ok();
	assert_eq!(atom.properties().get(age), Some(&Value::from(31)));
	assert_eq!(after.indexes.base_class(atom.class_id()).len(), 2);
	assert_eq!(after.indexes.graph_class(graph, atom.class_id()).len(), 2);
	assert_eq!(after.indexes.graph_label(graph, AtomKind::Node, &"Member".into()).len(), 2);
	assert!(after.statistics.base().dirty_rows > before.statistics.base().dirty_rows);
	let mut tx = store.begin();
	tx.set_overlay(graph, &first, note, "local-note".into()).assert_ok();
	tx.commit().assert_ok();
	let overlay = store.inner.current_root();
	assert!(Arc::ptr_eq(&after.indexes, &overlay.indexes));
	assert_eq!(
		store.begin().effective_property(graph, &first, note).assert_ok(),
		Some("local-note".into())
	);
	let old = after
		.statistics
		.graph(graph)
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument("test graph statistics absent")
		})
		.assert_ok();
	let new = overlay
		.statistics
		.graph(graph)
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument("test graph statistics absent")
		})
		.assert_ok();
	assert!(new.dirty_rows > old.dirty_rows);
}

/// Equal/unrelated/masked keys stay shared; real BASE/EFFECTIVE keys detach.
#[test]
fn empty_replacement_preserves_count_and_masked_effective_keys() {
	use crate::test_assertions::AssertResult as _;

	let (store, graph, [age, _email, nick, note]) = replacement_fixture(true).assert_ok();
	let first = record("first").assert_ok();
	let before = store.inner.current_root();
	let mut tx = store.begin();
	tx.set_base_property(&first, note, "unrelated".into()).assert_ok();
	tx.set_base_property(&first, age, 30.into()).assert_ok();
	tx.commit().assert_ok();
	let equal = store.inner.current_root();
	assert!(Arc::ptr_eq(&before.indexes, &equal.indexes));
	let mut tx = store.begin();
	tx.set_base_property(&first, age, 31.into()).assert_ok();
	tx.commit().assert_ok();
	let changed = store.inner.current_root();
	assert!(!Arc::ptr_eq(&equal.indexes, &changed.indexes));
	let old_keys = equal
		.indexes
		.base
		.values()
		.find(|index| index.name.as_str() == "age_idx")
		.and_then(|index| index.entries.scalar_entries());
	let new_keys = changed
		.indexes
		.base
		.values()
		.find(|index| index.name.as_str() == "age_idx")
		.and_then(|index| index.entries.scalar_entries());
	assert_ne!(old_keys, new_keys);
	assert_eq!(
		changed
			.indexes
			.base
			.values()
			.find(|index| index.name.as_str() == "count")
			.and_then(|index| index.entries.count()),
		Some(2)
	);
	let mut tx = store.begin();
	tx.set_overlay(graph, &first, nick, "masked".into()).assert_ok();
	tx.commit().assert_ok();
	let masked = store.inner.current_root();
	let mut tx = store.begin();
	tx.set_base_property(&first, nick, "new-base".into()).assert_ok();
	tx.commit().assert_ok();
	let base_changed = store.inner.current_root();
	assert!(Arc::ptr_eq(&masked.indexes, &base_changed.indexes));
	assert_eq!(
		store.begin().effective_property(graph, &first, nick).assert_ok(),
		Some("masked".into())
	);
	let mut tx = store.begin();
	tx.reset_property(graph, &first, nick).assert_ok();
	tx.commit().assert_ok();
	let inherited = store.inner.current_root();
	assert!(!Arc::ptr_eq(&base_changed.indexes, &inherited.indexes));
	assert_eq!(
		store.begin().effective_property(graph, &first, nick).assert_ok(),
		Some("new-base".into())
	);
}

/// Nonempty replacement remains staged through unique and late statistics
/// errors.
#[test]
fn empty_replacement_guard_keeps_unique_and_statistics_failures_atomic() {
	use crate::test_assertions::AssertResult as _;

	let (store, graph, [age, email, nick, _note]) = replacement_fixture(true).assert_ok();
	let before = store.inner.current_root();
	let global = before
		.resolve_atom(&record("first").assert_ok())
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("test atom absent"))
		.assert_ok();
	let local = before
		.memberships(global)
		.and_then(|memberships| memberships.get(&graph))
		.copied()
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument("test membership absent")
		})
		.assert_ok();
	let mut candidate = (*before).clone();
	let error = candidate
		.set_base_property(global, email, "email-b".into())
		.err()
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument("unique BASE conflict absent")
		})
		.assert_ok();
	assert_eq!(error.status().code().to_string(), "FG-G2F04");
	assert!(Arc::ptr_eq(&before.indexes, &candidate.indexes));
	assert!(Arc::ptr_eq(&before.atoms, &candidate.atoms));
	let error = candidate
		.set_overlay(graph, local, nick, "nick-b".into())
		.err()
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument(
				"unique EFFECTIVE conflict absent",
			)
		})
		.assert_ok();
	assert_eq!(error.status().code().to_string(), "FG-G2F04");
	assert!(Arc::ptr_eq(&before.indexes, &candidate.indexes));
	assert!(Arc::ptr_eq(&before.graphs[&graph], &candidate.graphs[&graph]));
	candidate.statistics = Arc::new(crate::stats::StatisticsStore::default());
	assert!(candidate.set_base_property(global, age, 31.into()).is_err());
	assert!(
		Arc::ptr_eq(&before.indexes, &candidate.indexes),
		"index preparation must remain unpublished on later failure"
	);
	assert!(Arc::ptr_eq(&before.atoms, &candidate.atoms));
	assert!(Arc::ptr_eq(&before, &store.inner.current_root()));
}

/// Already-stale deferred definitions still notify catch-up without copying
/// unchanged bindings; immediate-only schemas do not detach either catalog.
#[test]
fn empty_deferred_invalidation_keeps_sharing_and_reports_stale_definitions() {
	use crate::test_assertions::AssertResult as _;

	let (store, _graph, _keys) = replacement_fixture(true).assert_ok();
	let before = store.inner.current_root();
	let mut candidate = (*before).clone();
	assert!(candidate.mark_deferred_indexes_stale().assert_ok().is_empty());
	assert!(Arc::ptr_eq(&before.indexes, &candidate.indexes));
	assert!(Arc::ptr_eq(&before.schema, &candidate.schema));
	let class_id = before
		.atom_by_record(&record("first").assert_ok())
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("test atom absent"))
		.assert_ok()
		.class_id();
	let schema = Arc::make_mut(&mut candidate.schema);
	let class = Arc::make_mut(
		schema
			.classes
			.get_mut(&class_id)
			.ok_or_else(|| {
				crate::gql_status::FerrosGrynnError::invalid_argument("test class absent")
			})
			.assert_ok(),
	);
	let index = class
		.indexes
		.iter_mut()
		.find(|index| index.name().as_str() == "age_idx")
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("test index absent"))
		.assert_ok();
	*index = index
		.clone()
		.with_maintenance_mode(crate::datastore::IndexMaintenanceMode::Deferred)
		.assert_ok();
	let id = index
		.id()
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument("test index identity absent")
		})
		.assert_ok();
	let first = candidate.mark_deferred_indexes_stale().assert_ok();
	assert_eq!(first.len(), 1);
	assert_eq!(first[0].1, id);
	assert!(candidate.indexes.base_index(id).is_none());
	let bindings = Arc::clone(&candidate.indexes);
	assert_eq!(candidate.mark_deferred_indexes_stale().assert_ok(), first);
	assert!(Arc::ptr_eq(&bindings, &candidate.indexes));
}

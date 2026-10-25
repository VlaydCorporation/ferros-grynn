//! Recursive logical write-set limits apply to retained SDK mutations.

use super::*;
use crate::{
	datastore::InMemoryOptions,
	test_assertions::{AssertOption as _, AssertResult as _},
	values::Array,
};

/// Builds a committed source independently of the tested transaction budget.
fn limited_source(maximum: usize) -> (Datastore, RecordId, GraphRef, PropertyKeyId) {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	let graph = seed.define_graph("g", GraphKind::Graph).assert_ok();
	let key = seed.intern_property("payload").assert_ok();
	let record = "item:existing".parse::<RecordId>().assert_ok();
	let mut properties = PropertyMap::new();
	properties.insert(key, Value::from("base"));
	seed.create_atom(record.clone(), properties).assert_ok();
	seed.attach_node(graph.id(), &record, None, None).assert_ok();
	seed.commit().assert_ok();
	let root = (*store.inner.current_root()).clone();
	let limited = Datastore::from_root_for_test(
		InMemoryOptions {
			max_write_set_bytes: maximum,
			..InMemoryOptions::default()
		},
		root,
	);
	(limited, record, graph, key)
}

/// Exercises the three label-producing SDK paths against the same payload
/// limit.
fn assert_label_limit_rejects_mutation(kind: Option<AtomKind>) {
	let label = Text::from("x".repeat(16 * 1024));
	let maximum = label.estimated_heap_bytes() * 3;
	let (source, record, graph, _) = limited_source(1024 * 1024);
	let mut seed = source.begin();
	let other = seed.define_graph("other", GraphKind::Graph).assert_ok();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let edge = "link:r".parse::<RecordId>().assert_ok();
	seed.create_atom(edge.clone(), PropertyMap::new()).assert_ok();
	seed.commit().assert_ok();
	let root = source.inner.current_root();
	let global = root.atom_by_record(&record).assert_some().id();
	let node = root.membership(graph.id(), global).assert_some();
	let edge_global = root.atom_by_record(&edge).assert_some().id();
	let limited = Datastore::from_root_for_test(
		InMemoryOptions {
			max_write_set_bytes: maximum,
			..InMemoryOptions::default()
		},
		root.as_ref().clone(),
	);
	let mut write = limited.begin();
	let result = match kind {
		None => write.set_occurrence_label(graph.id(), &record, Some(label)),
		Some(AtomKind::Node) => write.attach_node(other.id(), &record, Some(label), None).map(drop),
		Some(AtomKind::Edge) => write
			.attach_edge(
				graph.id(),
				&edge,
				Some(label),
				EdgeTopology::directed([node], [node]),
				None,
				None,
			)
			.map(drop),
	};
	assert_eq!(result.assert_err().status().code().to_string(), "FG-54F07");
	assert!(write.working.occurrence(graph.id(), node).assert_some().label().is_none());
	assert!(write.working.membership(other.id(), global).is_none());
	assert!(write.working.membership(graph.id(), edge_global).is_none());
	for graph in [graph.id(), other.id()] {
		assert!(write.working.indexes.graphs[&graph].labels.is_empty());
		assert_eq!(write.working.statistics.graph(graph), root.statistics.graph(graph));
	}
	assert!(write.mutations.is_empty());
	assert!(write.writes.is_empty());
	assert_eq!(write.write_set_bytes(), 0);
	write.commit().assert_ok();
	assert_eq!(limited.inner.current_root().commit_ts, root.commit_ts);
}

/// A replacement owns lookup/statistics keys in addition to its source payload.
#[test]
fn write_set_label_replacement_rejects_before_private_mutation() {
	assert_label_limit_rejects_mutation(None);
}

/// Node attachment must admit all label owners before allocating membership.
#[test]
fn write_set_label_node_attach_rejects_before_private_mutation() {
	assert_label_limit_rejects_mutation(Some(AtomKind::Node));
}

/// Edge topology and label owners share one preflight budget.
#[test]
fn write_set_label_edge_attach_rejects_before_private_mutation() {
	assert_label_limit_rejects_mutation(Some(AtomKind::Edge));
}

/// Independent heap owners remain covered even if cached charges are bypassed.
#[test]
fn write_set_label_audit_counts_lookup_and_statistics_payload_owners() {
	let (store, record, graph, _) = limited_source(1024 * 1024);
	let label = Text::from("x".repeat(16 * 1024));
	let mut write = store.begin();
	write.set_occurrence_label(graph.id(), &record, Some(label.clone())).assert_ok();
	let journal = write
		.mutations
		.iter()
		.find_map(|mutation| match mutation {
			Mutation::SetOccurrenceLabel {
				label: Some(label),
				..
			} => Some(label),
			_ => None,
		})
		.assert_some();
	let local = write
		.working
		.membership(graph.id(), write.working.atom_by_record(&record).assert_some().id())
		.assert_some();
	let occurrence =
		write.working.occurrence(graph.id(), local).assert_some().label().assert_some();
	let lookup = &write.working.indexes.graphs[&graph.id()].labels.keys().next().assert_some().1;
	let statistics = &write
		.working
		.statistics
		.graph(graph.id())
		.assert_some()
		.labels
		.keys()
		.next()
		.assert_some()
		.label;
	let owners = [journal, occurrence, lookup, statistics];
	assert!(owners.iter().all(|label| label.estimated_heap_bytes() > 0));
	assert_eq!(
		owners
			.iter()
			.map(|label| label.as_str().as_ptr().addr())
			.collect::<std::collections::BTreeSet<_>>()
			.len(),
		4
	);
	let payload = owners.iter().map(|label| label.estimated_heap_bytes()).sum::<usize>();
	assert!(write.write_set_bytes() >= payload);
	write.payload_bytes = 0;
	assert!(write.recursive_write_set_bytes() >= payload);
	let limited = Datastore::from_root_for_test(
		InMemoryOptions {
			max_write_set_bytes: payload - label.estimated_heap_bytes(),
			..InMemoryOptions::default()
		},
		write.snapshot.as_ref().clone(),
	);
	write.store = limited.clone();
	assert_eq!(
		write
			.validate_candidate_payload(
				&write.snapshot,
				&write.working,
				&super::commit::ReplayRemap::default(),
			)
			.assert_err()
			.status()
			.code()
			.to_string(),
		"FG-54F07"
	);
	let stamp = limited.inner.current_root().commit_ts;
	assert_eq!(write.commit().assert_err().status().code().to_string(), "FG-54F07");
	assert_eq!(limited.inner.current_root().commit_ts, stamp);
}

/// Removing a label still pays for all retained source-side owners.
#[test]
fn write_set_label_reset_audits_the_old_baseline_before_publication() {
	let (source, record, graph, _) = limited_source(1024 * 1024);
	let label = Text::from("x".repeat(16 * 1024));
	let mut seed = source.begin();
	seed.set_occurrence_label(graph.id(), &record, Some(label.clone())).assert_ok();
	seed.commit().assert_ok();
	let root = source.inner.current_root();
	let limited = Datastore::from_root_for_test(
		InMemoryOptions {
			max_write_set_bytes: label.estimated_heap_bytes() * 2,
			..InMemoryOptions::default()
		},
		root.as_ref().clone(),
	);
	let mut write = limited.begin();
	write.set_occurrence_label(graph.id(), &record, None).assert_ok();
	assert_eq!(write.commit().assert_err().status().code().to_string(), "FG-54F07");
	assert_eq!(limited.inner.current_root().commit_ts, root.commit_ts);
}

/// Repeated operands stay charged; an equal-label call retains only its intent.
#[test]
fn write_set_label_repeated_targets_keep_journal_and_final_sidecars() {
	let (store, record, graph, _) = limited_source(1024 * 1024);
	let mut write = store.begin();
	for label in [Text::from("a".repeat(16 * 1024)), Text::from("b".repeat(24 * 1024))] {
		write.set_occurrence_label(graph.id(), &record, Some(label)).assert_ok();
	}
	let bytes = write.write_set_bytes();
	let journal_bytes = write.change_journal.bytes;
	let payload_bytes = write.payload_bytes;
	let journal = write
		.mutations
		.iter()
		.filter_map(|mutation| match mutation {
			Mutation::SetOccurrenceLabel {
				label: Some(label),
				..
			} => Some(label.estimated_heap_bytes()),
			_ => None,
		})
		.sum::<usize>();
	let label = Text::from("b".repeat(24 * 1024));
	assert!(write.recursive_write_set_bytes() >= journal + 3 * label.estimated_heap_bytes());
	let count = write.mutations.len();
	write.set_occurrence_label(graph.id(), &record, Some(label)).assert_ok();
	assert_eq!(write.payload_bytes, payload_bytes);
	// FULL changefeed retains the successful target even when the label is
	// unchanged.
	assert_eq!(write.write_set_bytes() - write.change_journal.bytes, bytes - journal_bytes);
	assert!(write.change_journal.intents.last().assert_some().noop);
	assert_eq!(write.mutations.len(), count);
	write.commit().assert_ok();
}

/// Catalog identities and nested field types cannot bypass the journal limit.
#[test]
fn write_set_rejects_class_payload_before_private_schema_mutation() {
	for rich_type in [false, true] {
		let (store, _, _, _) = limited_source(4096);
		let mut write = store.begin();
		let dictionary = write.working.dictionary.len();
		let class = if rich_type {
			ClassSchema::new("large", AtomKind::Node, [crate::datastore::ClassField::new(
				"nested",
				crate::values::Type::RecordShape(vec![(
					"x".repeat(64 << 10).into(),
					crate::values::Type::Int,
				)]),
			)])
		} else {
			ClassSchema::new("x".repeat(64 << 10), AtomKind::Node, [])
		}
		.assert_ok();
		let name = class.name().clone();
		assert_eq!(write.define_class(class).assert_err().status().code().to_string(), "FG-54F07");
		assert!(write.working.schema.class(name.as_str()).is_none());
		assert_eq!(write.working.dictionary.len(), dictionary);
		assert_eq!(write.write_set_bytes(), 0);
		assert!(write.mutations.is_empty());
	}
	let (store, _, _, _) = limited_source(4096);
	let mut write = store.begin();
	let dictionary = write.working.dictionary.len();
	let error = write
		.define_field(
			"item",
			crate::datastore::ClassField::new(
				"nested",
				crate::values::Type::RecordShape(vec![(
					"x".repeat(64 << 10).into(),
					crate::values::Type::Int,
				)]),
			),
		)
		.assert_err();
	assert_eq!(error.status().code().to_string(), "FG-54F07");
	assert_eq!(write.working.dictionary.len(), dictionary);
	assert!(write.working.schema.class("item").assert_some().fields().next().is_none());
	assert_eq!(write.write_set_bytes(), 0);
	assert!(write.mutations.is_empty());
}

/// Source bodies and mapper replacements are retained journal operands too.
#[test]
fn write_set_rejects_function_and_analyzer_payload_before_private_mutation() {
	let (store, _, _, _) = limited_source(4096);
	let mut write = store.begin();
	let function = crate::datastore::SchemaFunction::new(
		"fn",
		"wide",
		Vec::new(),
		crate::values::Type::Int,
		crate::datastore::FunctionEffect::Pure,
		false,
		None,
		format!("RETURN 1;{}", " ".repeat(64 << 10)),
	)
	.assert_ok();
	assert_eq!(
		write.define_function(function).assert_err().status().code().to_string(),
		"FG-54F07"
	);
	assert!(write.working.schema.functions.is_empty());
	assert!(write.mutations.is_empty());
	assert_eq!(write.write_set_bytes(), 0);
	let dictionary = write.working.dictionary.len();
	let analyzer = crate::datastore::SchemaAnalyzer::new("wide", [], [
		crate::datastore::AnalyzerFilter::Mapper(
			[("fresh_key".into(), Some("x".repeat(64 << 10).into()))].into_iter().collect(),
		),
	])
	.assert_ok();
	assert_eq!(
		write.define_analyzer(analyzer).assert_err().status().code().to_string(),
		"FG-54F07"
	);
	assert!(write.working.schema.analyzers().next().is_none());
	assert_eq!(write.working.dictionary.len(), dictionary);
	assert!(write.mutations.is_empty());
	assert_eq!(write.write_set_bytes(), 0);
}

/// Retained accounting preserves the RAM SDK's independently accepted type
/// depth.
#[test]
fn write_set_retained_types_preserve_sdk_depth_and_count_spare_capacity() {
	let (store, _, _, _) = limited_source(1 << 20);
	let mut nested = crate::values::Type::Int;
	for _ in 0..80 {
		nested = crate::values::Type::Array(Box::new(nested));
	}
	let mut write = store.begin();
	write
		.define_class(
			ClassSchema::new("deep", AtomKind::Node, [crate::datastore::ClassField::new(
				"nested", nested,
			)])
			.assert_ok(),
		)
		.assert_ok();
	write.commit().assert_ok();
	let (store, _, _, _) = limited_source(4096);
	let mut types = Vec::with_capacity(4096);
	types.push(crate::values::Type::Int);
	let mut write = store.begin();
	let dictionary = write.working.dictionary.len();
	assert_eq!(
		write
			.define_field(
				"item",
				crate::datastore::ClassField::new("union", crate::values::Type::Union(types),)
			)
			.assert_err()
			.status()
			.code()
			.to_string(),
		"FG-54F07"
	);
	assert_eq!(write.working.dictionary.len(), dictionary);
	assert_eq!(write.write_set_bytes(), 0);
}

/// Individually admitted schema and dictionary writes must also fit together.
#[test]
fn write_set_class_rechecks_budget_after_field_interning() {
	let class = ClassSchema::new(
		"combined",
		AtomKind::Node,
		(0..8).map(|ordinal| {
			crate::datastore::ClassField::new(format!("fresh_{ordinal}"), crate::values::Type::Int)
		}),
	)
	.assert_ok();
	let (probe, _, _, _) = limited_source(1 << 20);
	let mut calibration = probe.begin();
	for field in class.fields() {
		calibration.intern_property(field.name().as_str()).assert_ok();
	}
	let class_bytes = SCHEMA_WRITE_BYTES
		+ Transaction::mutation_payload_charge(super::write_set::class_payload_bytes(&class));
	let maximum = class_bytes + calibration.write_set_bytes() - 1;
	assert!(class_bytes < maximum);
	assert!(calibration.write_set_bytes() < maximum);
	let (store, _, _, _) = limited_source(maximum);
	let mut write = store.begin();
	let dictionary = write.working.dictionary.len();
	assert_eq!(write.define_class(class).assert_err().status().code().to_string(), "FG-54F07");
	assert!(write.working.schema.class("combined").is_none());
	assert_eq!(write.working.dictionary.len(), dictionary);
	assert_eq!(write.write_set_bytes(), 0);
	assert!(write.mutations.is_empty());
}

/// An oversized create fails before retaining either the atom or its journal.
#[test]
fn write_set_rejects_recursive_create_payload_before_private_mutation() {
	for nested in [false, true] {
		let (store, _, _, key) = limited_source(4096);
		let record = "item:large".parse::<RecordId>().assert_ok();
		let mut properties = PropertyMap::new();
		let value = Value::from("x".repeat(64 * 1024));
		properties.insert(
			key,
			if nested {
				Value::Array(Array::from(vec![value]))
			} else {
				value
			},
		);
		let mut write = store.begin();
		let error = write.create_atom(record.clone(), properties).map(|_atom| ()).assert_err();
		assert_eq!(error.status().code().to_string(), "FG-54F07");
		assert!(write.atom(&record).assert_ok().is_none());
		assert_eq!(write.write_set_bytes(), 0);
		assert!(write.mutations.is_empty());
	}
}

/// Composite record identity bytes are subject to the same admission limit.
#[test]
fn write_set_rejects_composite_record_payload_before_private_mutation() {
	let (store, _, _, _) = limited_source(4096);
	let record = RecordId::new(
		"item",
		RecordIdPart::Array(Array::from(vec![Value::from("x".repeat(64 * 1024))])),
	)
	.assert_ok();
	let mut write = store.begin();
	let error = write.create_atom(record.clone(), PropertyMap::new()).map(|_atom| ()).assert_err();
	assert_eq!(error.status().code().to_string(), "FG-54F07");
	assert!(write.atom(&record).assert_ok().is_none());
	assert_eq!(write.write_set_bytes(), 0);
	let record = RecordId::new(
		"item",
		RecordIdPart::Object([(Text::from("k"), Value::from(1))].into_iter().collect()),
	)
	.assert_ok();
	let (tiny, _, _, _) = limited_source(1024);
	let mut write = tiny.begin();
	assert_eq!(
		write
			.create_atom(record.clone(), PropertyMap::new())
			.map(|_atom| ())
			.assert_err()
			.status()
			.code()
			.to_string(),
		"FG-54F07"
	);
	assert!(write.atom(&record).assert_ok().is_none());
	assert!(write.mutations.is_empty());
}

/// Base and overlay setters retain their preceding value after admission fails.
#[test]
fn write_set_rejects_large_setters_without_changing_private_values() {
	for overlay in [false, true] {
		let (store, record, graph, key) = limited_source(4096);
		let mut write = store.begin();
		let value = Value::from("x".repeat(64 * 1024));
		let result = if overlay {
			write.set_overlay(graph.id(), &record, key, value)
		} else {
			write.set_base_property(&record, key, value).map(|_previous| ())
		};
		assert_eq!(result.assert_err().status().code().to_string(), "FG-54F07");
		assert_eq!(write.write_set_bytes(), 0);
		assert!(write.mutations.is_empty());
		assert_eq!(
			write.effective_property(graph.id(), &record, key).assert_ok(),
			Some(Value::from("base"))
		);
	}
}

/// Replacing one target does not discard payloads retained by earlier
/// mutations.
#[test]
fn write_set_counts_every_retained_same_target_payload() {
	let (store, record, graph, key) = limited_source(16 * 1024);
	let mut write = store.begin();
	let mut rejected = false;
	let mut accepted = 0;
	for iteration in 0..16 {
		let bytes = write.write_set_bytes();
		let before = write.effective_property(graph.id(), &record, key).assert_ok();
		let value = Value::from(format!("{iteration}:{}", "x".repeat(1024)));
		match write.set_base_property(&record, key, value) {
			Ok(_previous) => {
				assert!(write.write_set_bytes() > bytes);
				accepted += 1;
			}
			Err(error) => {
				assert_eq!(error.status().code().to_string(), "FG-54F07");
				assert_eq!(write.write_set_bytes(), bytes);
				assert_eq!(write.effective_property(graph.id(), &record, key).assert_ok(), before);
				rejected = true;
				break;
			}
		}
	}
	assert!(rejected);
	assert!(accepted >= 2);
	assert_eq!(
		store.begin().atom(&record).assert_ok().assert_some().properties().get(key),
		Some(&Value::from("base"))
	);
}

/// The final guard catches an operand missing from cached admission.
#[test]
fn write_set_commit_rechecks_recursive_payload_before_publication() {
	let (store, record, _, key) = limited_source(4096);
	let stamp = store.inner.current_root().commit_ts;
	let mut write = store.begin();
	let global = write.working.resolve_atom(&record).assert_some();
	let value = Value::from("x".repeat(64 * 1024));
	write.working.set_base_property(global, key, value.clone()).assert_ok();
	write.mutations.push(Mutation::SetBaseProperty {
		global,
		key,
		value,
	});
	assert_eq!(write.write_set_bytes(), 0);
	assert_eq!(write.commit().assert_err().status().code().to_string(), "FG-54F07");
	assert_eq!(store.inner.current_root().commit_ts, stamp);
	assert_eq!(
		store.begin().atom(&record).assert_ok().assert_some().properties().get(key),
		Some(&Value::from("base"))
	);
}

/// A single new edge must account for complete existing endpoint adjacency.
#[test]
fn write_set_commit_counts_derived_adjacency_of_unchanged_edges() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = seed.define_graph("g", GraphKind::Graph).assert_ok();
	let nodes = ["item:a", "item:b"].map(|record| {
		let record = record.parse::<RecordId>().assert_ok();
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		seed.attach_node(graph.id(), &record, None, None).assert_ok()
	});
	for ordinal in 0..257 {
		let record = format!("link:r{ordinal}").parse::<RecordId>().assert_ok();
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		if ordinal < 256 {
			seed.attach_edge(
				graph.id(),
				&record,
				None,
				EdgeTopology::directed([nodes[0]], [nodes[1]]),
				None,
				None,
			)
			.assert_ok();
		}
	}
	seed.commit().assert_ok();
	let root = (*store.inner.current_root()).clone();
	let stamp = root.commit_ts;
	let limited = Datastore::from_root_for_test(
		InMemoryOptions {
			max_write_set_bytes: 4096,
			..InMemoryOptions::default()
		},
		root,
	);
	let mut write = limited.begin();
	write
		.attach_edge(
			graph.id(),
			&"link:r256".parse().assert_ok(),
			None,
			EdgeTopology::directed([nodes[0]], [nodes[1]]),
			None,
			None,
		)
		.assert_ok();
	assert!(write.write_set_bytes() <= 4096);
	assert_eq!(
		write.seal_import_segment(stamp).assert_err().status().code().to_string(),
		"FG-54F07"
	);
	assert!(!write.mutations.is_empty());
	assert_eq!(write.commit().assert_err().status().code().to_string(), "FG-54F07");
	assert_eq!(limited.inner.current_root().commit_ts, stamp);
}

/// Spare Array capacity belongs to retained values and composite identities.
#[test]
fn write_set_counts_empty_array_capacity() {
	let (store, record, _, key) = limited_source(4096);
	let mut write = store.begin();
	assert_eq!(
		write
			.set_base_property(&record, key, Value::Array(Array::with_capacity(4096)))
			.map(|_previous| ())
			.assert_err()
			.status()
			.code()
			.to_string(),
		"FG-54F07"
	);
	let record = RecordId::new("item", RecordIdPart::Array(Array::with_capacity(4096))).assert_ok();
	assert_eq!(
		write
			.create_atom(record, PropertyMap::new())
			.map(|_atom| ())
			.assert_err()
			.status()
			.code()
			.to_string(),
		"FG-54F07"
	);
	assert_eq!(write.write_set_bytes(), 0);
}

/// Empty and truncated SDK buffers still retain their allocated payload.
#[test]
fn write_set_counts_spare_binary_vector_geometry_and_label_capacity() {
	use crate::values::{Bytes, Geometry, Vector};

	let values = [
		Value::Bytes(Bytes::from(Vec::<u8>::with_capacity(64 * 1024))),
		Value::Vector(Vector::F32(Vec::with_capacity(16 * 1024))),
		Value::Geometry(Geometry::MultiLineString(Vec::with_capacity(4096))),
	];
	for value in values {
		let (store, record, _, key) = limited_source(4096);
		let mut write = store.begin();
		assert_eq!(
			write
				.set_base_property(&record, key, value)
				.map(|_old| ())
				.assert_err()
				.status()
				.code()
				.to_string(),
			"FG-54F07"
		);
		assert!(write.mutations.is_empty());
		assert_eq!(write.write_set_bytes(), 0);
	}
	let (store, record, graph, _) = limited_source(4096);
	let mut label = Text::from("x".repeat(64 * 1024));
	label.truncate(1).assert_ok();
	let mut write = store.begin();
	assert_eq!(
		write
			.set_occurrence_label(graph.id(), &record, Some(label))
			.assert_err()
			.status()
			.code()
			.to_string(),
		"FG-54F07"
	);
	assert!(write.mutations.is_empty());
	assert_eq!(write.write_set_bytes(), 0);
}

/// Public reference wrappers retain their graph names recursively.
#[test]
fn write_set_counts_graph_reference_name_capacity() {
	use crate::graph::{AtomOccurrenceRef, AtomRef};

	for occurrence in [false, true] {
		let (store, record, graph, key) = limited_source(4096);
		let mut name = Text::from("x".repeat(64 * 1024));
		name.truncate(1).assert_ok();
		let reference =
			GraphRef::new(graph.id(), name, graph.kind(), graph.storage_mode()).assert_ok();
		let value = if occurrence {
			Value::AtomOccurrence(Box::new(AtomOccurrenceRef::new(
				AtomRef::new(AtomKind::Node, record.clone()),
				Arc::new(reference),
			)))
		} else {
			Value::GraphRef(Box::new(reference))
		};
		let mut write = store.begin();
		assert_eq!(
			write
				.set_base_property(&record, key, value)
				.map(|_old| ())
				.assert_err()
				.status()
				.code()
				.to_string(),
			"FG-54F07"
		);
		assert!(write.mutations.is_empty());
	}
}

/// Nested sparse trees pay for node storage rather than logical entries alone.
#[test]
fn write_set_counts_sparse_object_and_set_nodes() {
	use crate::values::{Object, Set};

	for object in [false, true] {
		let children = (0..16)
			.map(|_| {
				if object {
					Value::Object(
						[(Text::from("k"), Value::from(1))].into_iter().collect::<Object>(),
					)
				} else {
					let mut values = Set::new();
					values.insert(1);
					Value::Set(values)
				}
			})
			.collect::<Vec<_>>();
		let (store, record, _, key) = limited_source(8192);
		let mut write = store.begin();
		assert_eq!(
			write
				.set_base_property(&record, key, Value::Array(Array::from(children)))
				.map(|_old| ())
				.assert_err()
				.status()
				.code()
				.to_string(),
			"FG-54F07"
		);
		assert!(write.mutations.is_empty());
	}
}

/// A successful private segment releases its ordinary mutation payload charges.
#[test]
fn write_set_segment_reset_releases_only_segment_operands() {
	let (store, record, _, key) = limited_source(64 * 1024);
	let mut write = store.begin();
	write.set_base_property(&record, key, Value::from("x".repeat(1024))).assert_ok();
	assert!(write.payload_bytes > 0);
	write.seal_import_segment(write.snapshot.commit_ts).assert_ok();
	assert_eq!(write.payload_bytes, 0);
	assert_eq!(write.write_set_bytes(), 0);
	assert!(write.mutations.is_empty());
}

/// Capability metadata must be admitted before changing an edge's private
/// source.
#[test]
fn write_set_capability_rejection_preserves_membership_and_topology() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let key = seed.intern_property("payload").assert_ok();
	let mut properties = PropertyMap::new();
	properties.insert(key, Value::from("x".repeat(8192)));
	let graph = seed.define_graph_with_properties("g", GraphKind::Graph, properties).assert_ok();
	let records =
		["item:a", "item:b", "link:base", "link:next"].map(|id| id.parse::<RecordId>().assert_ok());
	for record in &records {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let a = seed.attach_node(graph.id(), &records[0], None, None).assert_ok();
	let b = seed.attach_node(graph.id(), &records[1], None, None).assert_ok();
	let edge = seed
		.attach_edge(graph.id(), &records[2], None, EdgeTopology::directed([a], [b]), None, None)
		.assert_ok();
	seed.commit().assert_ok();
	let root = (*store.inner.current_root()).clone();
	let limited = Datastore::from_root_for_test(
		InMemoryOptions {
			max_write_set_bytes: 4096,
			..InMemoryOptions::default()
		},
		root,
	);
	for replace in [false, true] {
		let mut write = limited.begin();
		let before = write.working.occurrence(graph.id(), edge).assert_some().state().clone();
		let topology = EdgeTopology::directed([edge], [a]);
		let result = if replace {
			write.replace_occurrence_state(
				graph.id(),
				&records[2],
				OccurrenceState::edge(topology, None, None),
			)
		} else {
			write.attach_edge(graph.id(), &records[3], None, topology, None, None).map(|_local| ())
		};
		assert_eq!(result.assert_err().status().code().to_string(), "FG-54F07");
		assert_eq!(write.working.occurrence(graph.id(), edge).assert_some().state(), &before);
		let global = write.working.resolve_atom(&records[3]).assert_some();
		assert!(write.working.membership(graph.id(), global).is_none());
		assert!(write.mutations.is_empty());
	}
}

/// Explicit savepoints restore the admission counter with the mutation suffix.
#[test]
fn write_set_payload_savepoint_restores_prior_successful_mutations() {
	let (store, record, _, key) = limited_source(64 * 1024);
	let mut write = store.begin();
	write.set_base_property(&record, key, Value::from("first")).assert_ok();
	let bytes = write.write_set_bytes();
	let savepoint = write.gql_savepoint();
	write.set_base_property(&record, key, Value::from("x".repeat(4096))).assert_ok();
	assert!(write.write_set_bytes() > bytes);
	write.restore_gql_savepoint(savepoint);
	assert_eq!(write.write_set_bytes(), bytes);
	write.commit().assert_ok();
	assert_eq!(
		store.begin().atom(&record).assert_ok().assert_some().properties().get(key),
		Some(&Value::from("first"))
	);
}

/// Large graph metadata follows the same early payload admission boundary.
#[test]
fn write_set_rejects_graph_properties_before_private_replacement() {
	let (store, _, graph, key) = limited_source(4096);
	let mut properties = PropertyMap::new();
	properties.insert(key, Value::from("x".repeat(64 * 1024)));
	let mut write = store.begin();
	assert_eq!(
		write
			.replace_graph_properties(graph.name().as_str(), properties)
			.assert_err()
			.status()
			.code()
			.to_string(),
		"FG-54F07"
	);
	assert_eq!(write.write_set_bytes(), 0);
	assert!(write.mutations.is_empty());
}

/// Removing a graph must not hide the recursive payload of its former contents.
#[test]
fn write_set_commit_counts_payload_of_removed_graph_occurrences() {
	let (store, record, graph, key) = limited_source(256 * 1024);
	let mut seed = store.begin();
	seed.set_base_property(&record, key, Value::from("x".repeat(32 * 1024))).assert_ok();
	seed.commit().assert_ok();
	let root = (*store.inner.current_root()).clone();
	let stamp = root.commit_ts;
	let limited = Datastore::from_root_for_test(
		InMemoryOptions {
			max_write_set_bytes: 4096,
			..InMemoryOptions::default()
		},
		root,
	);
	let mut write = limited.begin();
	write.remove_graph(graph.name().as_str()).assert_ok();
	assert_eq!(write.commit().assert_err().status().code().to_string(), "FG-54F07");
	assert_eq!(limited.inner.current_root().commit_ts, stamp);
	assert!(limited.inner.current_root().schema.graph(graph.id()).is_some());
}

/// A rebased commit pays for adjacency added independently after its snapshot.
#[test]
fn write_set_commit_rechecks_concurrent_adjacency_growth() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = seed.define_graph("g", GraphKind::Graph).assert_ok();
	let nodes = ["item:a", "item:b"].map(|record| {
		let record = record.parse::<RecordId>().assert_ok();
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		seed.attach_node(graph.id(), &record, None, None).assert_ok()
	});
	let records = (0..257)
		.map(|ordinal| format!("link:r{ordinal}").parse::<RecordId>().assert_ok())
		.collect::<Vec<_>>();
	for record in &records {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	seed.commit().assert_ok();
	let mut stale = store.begin();
	stale
		.attach_edge(
			graph.id(),
			&records[256],
			None,
			EdgeTopology::directed([nodes[0]], [nodes[1]]),
			None,
			None,
		)
		.assert_ok();
	let mut other = store.begin();
	for record in &records[..256] {
		other
			.attach_edge(
				graph.id(),
				record,
				None,
				EdgeTopology::directed([nodes[0]], [nodes[1]]),
				None,
				None,
			)
			.assert_ok();
	}
	other.commit().assert_ok();
	let stamp = store.inner.current_root().commit_ts;
	let limited = Datastore::from_root_for_test(
		InMemoryOptions {
			max_write_set_bytes: 4096,
			..InMemoryOptions::default()
		},
		(*store.inner.current_root()).clone(),
	);
	stale.store = limited.clone();
	assert!(stale.recursive_write_set_bytes() < 4096);
	assert_eq!(stale.commit().assert_err().status().code().to_string(), "FG-54F07");
	assert_eq!(limited.inner.current_root().commit_ts, stamp);
}

/// Catalog parameters retain the same recursive runtime payload as atom values.
#[test]
fn write_set_rejects_parameter_payload_before_private_replacement() {
	let (store, _, _, _) = limited_source(4096);
	let mut seed = store.begin();
	seed.define_parameter(SchemaParameter::new("p", Value::from(1)).assert_ok()).assert_ok();
	seed.commit().assert_ok();
	for replace in [false, true] {
		let mut write = store.begin();
		let value = Value::from("x".repeat(64 * 1024));
		let result = if replace {
			write.replace_parameter("p", Some(value), None)
		} else {
			write.define_parameter(SchemaParameter::new("new", value).assert_ok())
		};
		assert_eq!(result.assert_err().status().code().to_string(), "FG-54F07");
		assert!(write.mutations.is_empty());
		assert_eq!(write.working.schema.parameter("p").assert_some().value(), &Value::from(1));
		assert!(write.working.schema.parameter("new").is_none());
	}
}

use super::{build_scoped_index_images, validate_scoped_index_images};
use crate::{
	datastore::{ClassField, ClassIndex, ClassIndexKind, ClassSchema, Datastore, IndexScope},
	graph::{AtomKind, GraphKind},
	values::{PropertyMap, RecordId, RecordIdPart, Type},
};

/// Verifies scoped images round trip base effective class label and count.
#[test]
fn scoped_images_round_trip_base_effective_class_label_and_count() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	let graph = transaction.define_graph("social", GraphKind::Graph).assert_ok();
	let class = ClassSchema::new("person", AtomKind::Node, [ClassField::new("age", Type::Int)])
		.assert_ok()
		.with_index(
			ClassIndex::new("by_age", ClassIndexKind::Standard, IndexScope::Base, ["age"])
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
				"effective_age",
				ClassIndexKind::Unique,
				IndexScope::Effective {
					graph: graph.id(),
				},
				["age"],
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
	transaction.define_class(class).assert_ok();
	transaction.commit().assert_ok();
	let schema = store
		.class("person")
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::class_does_not_exist("person"))
		.assert_ok();
	let key = schema
		.fields()
		.find(|field| field.name().as_str() == "age")
		.and_then(|field| field.key)
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("age key absent"))
		.assert_ok();
	let record = RecordId::new("person", RecordIdPart::String("alice".into())).assert_ok();
	let mut transaction = store.begin();
	let mut properties = PropertyMap::new();
	properties.insert(key, 42_i64);
	transaction.create_atom(record.clone(), properties).assert_ok();
	transaction.attach_node(graph.id(), &record, Some("Employee".into()), None).assert_ok();
	transaction.commit().assert_ok();

	let root = store.inner.current_root();
	let images = build_scoped_index_images(&root).assert_ok();
	validate_scoped_index_images(&images, &root).assert_ok();
	assert!(images.base.iter().any(|file| file.relative_path == "idx/std_0000.fgidx"));
	assert!(images.base.iter().any(|file| file.relative_path.starts_with("idx/cnt_")));
	let graph_files = images
		.graphs
		.get(&graph.id())
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_storage_file("graph image absent")
		})
		.assert_ok();
	assert!(graph_files.iter().any(|file| file.relative_path.starts_with("idx/cls_")));
	assert!(graph_files.iter().any(|file| file.relative_path == "idx/lbl_node_0.fgidx"));
	assert!(graph_files.iter().any(|file| file.relative_path == "idx/uniq_0000.fgidx"));
}

/// A reused NODE slot sorts differently in logical and generation-free order.
#[test]
fn scoped_class_images_accept_reused_node_generation_and_reject_missing_membership() {
	reused_class(AtomKind::Node);
}

/// EDGE class postings retain the same slot ordering after local generation
/// reuse.
#[test]
fn scoped_class_images_accept_reused_edge_generation_and_reject_missing_membership() {
	reused_class(AtomKind::Edge);
}

/// Exercises the actual SDK before building and independently validating pages.
fn reused_class(kind: AtomKind) {
	use crate::{
		graph::EdgeTopology,
		test_assertions::{AssertOption as _, AssertResult as _},
	};
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("node", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("edge", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = seed.define_graph("g", GraphKind::Graph).assert_ok();
	let nodes = ["node:a", "node:b"].map(|name| {
		let record = name.parse::<RecordId>().assert_ok();
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		seed.attach_node(graph.id(), &record, None, None).assert_ok()
	});
	let record = match kind {
		AtomKind::Node => "node:a".parse::<RecordId>().assert_ok(),
		AtomKind::Edge => {
			for name in ["edge:a", "edge:b"] {
				let record = name.parse::<RecordId>().assert_ok();
				seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
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
			"edge:a".parse::<RecordId>().assert_ok()
		}
	};
	seed.commit().assert_ok();
	let mut reuse = store.begin();
	reuse.detach_occurrence(graph.id(), &record, false).assert_ok();
	let local = match kind {
		AtomKind::Node => reuse.attach_node(graph.id(), &record, None, None).assert_ok(),
		AtomKind::Edge => reuse
			.attach_edge(
				graph.id(),
				&record,
				None,
				EdgeTopology::directed([nodes[0]], [nodes[1]]),
				None,
				None,
			)
			.assert_ok(),
	};
	assert_eq!((local.slot(), local.generation()), (0, 1));
	reuse.commit().assert_ok();
	let root = store.inner.current_root();
	let images = build_scoped_index_images(&root).assert_ok();
	validate_scoped_index_images(&images, &root).assert_ok();
	let mut missing = root.as_ref().clone();
	missing.detach(graph.id(), root.resolve_atom(&record).assert_some()).assert_ok().assert_some();
	validate_scoped_index_images(&images, &missing).assert_err();
}

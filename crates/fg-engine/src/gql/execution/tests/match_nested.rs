#[cfg(test)]
use crate::test_assertions::AssertOption as _;
#[cfg(test)]
use crate::test_assertions::AssertResult as _;
use crate::{
	datastore::{
		ClassSchema,
		Datastore,
		IsolationLevel,
		Transaction,
		TransactionAccess,
		TransactionOptions,
	},
	graph::{AtomKind, EdgeEndpoint, EdgeTopology, GraphKind, QualifiedAtomId},
	values::{Object, PropertyMap, RecordId},
};

/// Returns row count for this module.
fn row_count(transaction: &mut Transaction, source: &str) -> usize {
	transaction.execute(source, &Object::new()).assert_ok()[0]
		.result
		.as_ref()
		.assert_ok()
		.value()
		.as_array()
		.assert_some()
		.len()
}

/// Verifies meta scopes apply to their complete position domains.
#[test]
fn meta_scopes_apply_to_their_complete_position_domains() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("module", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("bridge", AtomKind::Edge, []).assert_ok()).assert_ok();
	let child_a = seed.define_graph("child_a", GraphKind::Graph).assert_ok();
	let child_b = seed.define_graph("child_b", GraphKind::Graph).assert_ok();
	let transition = seed.define_graph("transition", GraphKind::Graph).assert_ok();
	let meta = seed.define_graph("scope_meta", GraphKind::Metagraph).assert_ok();
	let a = "module:a".parse::<RecordId>().assert_ok();
	let b = "module:b".parse::<RecordId>().assert_ok();
	let plain = "module:plain".parse::<RecordId>().assert_ok();
	let meta_edge = "bridge:meta".parse::<RecordId>().assert_ok();
	let plain_edge = "bridge:plain".parse::<RecordId>().assert_ok();
	for record in [&a, &b, &plain, &meta_edge, &plain_edge] {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let a_local = seed.attach_node(meta.id(), &a, None, Some(child_a.id())).assert_ok();
	let b_local = seed.attach_node(meta.id(), &b, None, Some(child_b.id())).assert_ok();
	seed.attach_node(meta.id(), &plain, None, None).assert_ok();
	seed.attach_edge(
		meta.id(),
		&meta_edge,
		None,
		EdgeTopology::directed([a_local], [b_local]),
		None,
		Some(transition.id()),
	)
	.assert_ok();
	seed.attach_edge(
		meta.id(),
		&plain_edge,
		None,
		EdgeTopology::directed([a_local], [b_local]),
		None,
		None,
	)
	.assert_ok();
	seed.commit().assert_ok();

	let mut transaction = store.begin();
	assert_eq!(
		row_count(&mut transaction, "MATCH METAVERTEX (module AS m) IN GRAPH scope_meta RETURN m",),
		2,
	);
	assert_eq!(
		row_count(
			&mut transaction,
			"MATCH METAEDGE (module:a AS a)-[bridge AS e]->(module:b AS b) IN GRAPH scope_meta \
			 RETURN e",
		),
		1,
	);
	assert_eq!(
		row_count(
			&mut transaction,
			"MATCH META (module AS a)-[bridge AS e]->(module AS b) IN GRAPH scope_meta RETURN e",
		),
		1,
	);
	assert_eq!(
		row_count(
			&mut transaction,
			"MATCH <(module AS a)>-[bridge AS e]-><(module AS b)> IN GRAPH scope_meta RETURN e",
		),
		2,
		"local meta endpoint wrappers must not require a meta connector",
	);
}

/// Verifies within is bounded transitive and preserves multiple owners.
#[test]
fn within_is_bounded_transitive_and_preserves_multiple_owners() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("module", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("document", AtomKind::Node, []).assert_ok()).assert_ok();
	let leaf_graph = seed.define_graph("within_leaf", GraphKind::Graph).assert_ok();
	let middle_graph = seed.define_graph("within_middle", GraphKind::Metagraph).assert_ok();
	let root_graph = seed.define_graph("within_root", GraphKind::Metagraph).assert_ok();
	let leaf = "document:leaf".parse::<RecordId>().assert_ok();
	let middle = "module:middle".parse::<RecordId>().assert_ok();
	let back = "module:back".parse::<RecordId>().assert_ok();
	let first = "module:first".parse::<RecordId>().assert_ok();
	let second = "module:second".parse::<RecordId>().assert_ok();
	for record in [&leaf, &middle, &back, &first, &second] {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	seed.attach_node(leaf_graph.id(), &leaf, None, None).assert_ok();
	seed.attach_node(middle_graph.id(), &middle, None, Some(leaf_graph.id())).assert_ok();
	seed.attach_node(middle_graph.id(), &back, None, Some(root_graph.id())).assert_ok();
	seed.attach_node(root_graph.id(), &first, None, Some(middle_graph.id())).assert_ok();
	seed.attach_node(root_graph.id(), &second, None, Some(middle_graph.id())).assert_ok();
	seed.commit().assert_ok();

	let mut transaction = store.begin();
	assert_eq!(
		row_count(
			&mut transaction,
			"MATCH (document AS d) WITHIN <(module AS owner)> IN GRAPH within_root RETURN d, owner",
		),
		0,
	);
	assert_eq!(
		row_count(
			&mut transaction,
			"MATCH (document AS d) WITHIN*[2..2] <(module AS owner)> IN GRAPH within_root RETURN \
			 d, owner",
		),
		2,
		"both root owners reach the same leaf through their own containment chain",
	);
	assert_eq!(
		row_count(
			&mut transaction,
			"MATCH (module:middle AS c) WITHIN*[1..7] <(module:first AS owner)> IN GRAPH \
			 within_root RETURN c, owner",
		),
		2,
		"a containment cycle terminates after distinct owner occurrences",
	);
	let overflow = transaction
		.execute(
			"MATCH (document AS d) WITHIN*[1..8] <(module AS owner)> IN GRAPH within_root RETURN d",
			&Object::new(),
		)
		.assert_ok();
	assert!(overflow[0].result.is_err());
	let zero = transaction
		.execute(
			"MATCH (document AS d) WITHIN*[0..1] <(module AS owner)> IN GRAPH within_root RETURN d",
			&Object::new(),
		)
		.assert_ok();
	assert!(zero[0].result.is_err());
	drop(transaction);

	let mut snapshot = store
		.begin_with(TransactionOptions::new(IsolationLevel::Snapshot, TransactionAccess::ReadOnly));
	let query = "MATCH (document AS d) WITHIN*[2..2] <(module AS owner)> IN GRAPH within_root \
	             RETURN d, owner";
	assert_eq!(row_count(&mut snapshot, query), 2);
	let mut writer = store.begin();
	let third = "module:third".parse::<RecordId>().assert_ok();
	writer.create_atom(third.clone(), PropertyMap::new()).assert_ok();
	writer.attach_node(root_graph.id(), &third, None, Some(middle_graph.id())).assert_ok();
	writer.commit().assert_ok();
	assert_eq!(row_count(&mut snapshot, query), 2, "the statement snapshot must not rebind graphs");
	assert_eq!(row_count(&mut store.begin(), query), 3);
}

/// Verifies cross level filter accepts only the selected nested branch.
#[test]
fn cross_level_filter_accepts_only_the_selected_nested_branch() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("module", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("document", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("bridge", AtomKind::Edge, []).assert_ok()).assert_ok();
	let child = seed.define_graph("filter_child", GraphKind::Graph).assert_ok();
	let meta = seed.define_graph("filter_meta", GraphKind::Metagraph).assert_ok();
	let module = "module:root".parse::<RecordId>().assert_ok();
	let nested_document = "document:nested".parse::<RecordId>().assert_ok();
	let local_document = "document:local".parse::<RecordId>().assert_ok();
	let nested_bridge = "bridge:nested".parse::<RecordId>().assert_ok();
	let local_bridge = "bridge:local".parse::<RecordId>().assert_ok();
	for record in [&module, &nested_document, &local_document, &nested_bridge, &local_bridge] {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let child_document = seed.attach_node(child.id(), &nested_document, None, None).assert_ok();
	let owner = seed.attach_node(meta.id(), &module, None, Some(child.id())).assert_ok();
	let local = seed.attach_node(meta.id(), &local_document, None, None).assert_ok();
	seed.attach_edge(
		meta.id(),
		&nested_bridge,
		None,
		EdgeTopology::directed([EdgeEndpoint::Local(owner)], [EdgeEndpoint::Nested(
			QualifiedAtomId::new([owner], child_document).assert_ok(),
		)]),
		None,
		None,
	)
	.assert_ok();
	seed.attach_edge(
		meta.id(),
		&local_bridge,
		None,
		EdgeTopology::directed([owner], [local]),
		None,
		None,
	)
	.assert_ok();
	seed.commit().assert_ok();

	let mut transaction = store.begin();
	assert_eq!(
		row_count(
			&mut transaction,
			"MATCH (module:root AS m)-[bridge]->(document AS d) IN GRAPH filter_meta RETURN d",
		),
		2,
	);
	assert_eq!(
		row_count(
			&mut transaction,
			"MATCH CONSTRAINTS <CROSS_LEVEL> (module:root AS m)-[bridge]->(document AS d) IN \
			 GRAPH filter_meta RETURN d",
		),
		1,
	);
	let analytics = "TRAVERSE FROM MATCH (module:root AS m)-[bridge]->(document AS d) IN GRAPH \
	                 filter_meta RETURN d |> tv::degree({scope:'ROOTED'});";
	assert_eq!(row_count(&mut transaction, analytics), 1);
	for order in ["ORDER BY d.id", "ORDER BY d.id DESC"] {
		assert_eq!(
			row_count(
				&mut transaction,
				&analytics.replace("RETURN d |> ", &format!("RETURN d {order} |> "))
			),
			1
		);
	}
	let mut parameters = Object::new();
	parameters.insert(
		"target",
		crate::values::Value::from(crate::graph::AtomOccurrenceRef::new(
			crate::graph::AtomRef::new(AtomKind::Node, module),
			std::sync::Arc::new(meta.clone()),
		)),
	);
	let paths = "TRAVERSE FROM MATCH (module:root AS m)-[bridge]->(document AS d) IN GRAPH \
	             filter_meta RETURN d |> \
	             tv::allShortestPaths($target,{scope:'ROOTED',direction:'IN',max_depth:1});";
	let results = transaction.execute(paths, &parameters).assert_ok();
	assert_eq!(results[0].result.as_ref().assert_ok().value().as_array().assert_some().len(), 2);
	drop(transaction);
	let mut seed = store.begin();
	let cache = seed
		.define_graph_index(
			"filter_meta",
			crate::datastore::GraphIndex::new_tensor(
				"rooted",
				crate::graph::tensor::TensorScope::Rooted,
				crate::datastore::TensorCacheFormat::Csr,
				1 << 20,
			)
			.assert_ok(),
		)
		.assert_ok();
	seed.commit().assert_ok();
	store
		.rebuild_graph_index(meta.id(), cache, crate::datastore::MaintenanceOptions::default())
		.assert_ok();
	let explained = store
		.execute(&format!("EXPLAIN ANALYZE FORMAT JSON {analytics}"), &Object::new())
		.assert_ok();
	let metrics = explained[0]
		.result
		.as_ref()
		.assert_ok()
		.value()
		.as_object()
		.assert_some()
		.get("analyze")
		.assert_some()
		.as_object()
		.assert_some();
	assert_eq!(metrics.get("tensor_cache_hits"), Some(&crate::values::Value::from(1_u64)));
	let explained =
		store.execute(&format!("EXPLAIN ANALYZE FORMAT JSON {paths}"), &parameters).assert_ok();
	let metrics = explained[0]
		.result
		.as_ref()
		.assert_ok()
		.value()
		.as_object()
		.assert_some()
		.get("analyze")
		.assert_some()
		.as_object()
		.assert_some();
	assert_eq!(metrics.get("tensor_cache_hits"), Some(&crate::values::Value::from(2_u64)));
}

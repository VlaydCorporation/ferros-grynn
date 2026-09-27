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

fn row_count(transaction: &mut Transaction, source: &str) -> usize {
	transaction.execute(source, &Object::new()).unwrap()[0]
		.result
		.as_ref()
		.unwrap()
		.value()
		.as_array()
		.unwrap()
		.len()
}

#[test]
fn meta_scopes_apply_to_their_complete_position_domains() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("module", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("bridge", AtomKind::Edge, []).unwrap()).unwrap();
	let child_a = seed.define_graph("child_a", GraphKind::Graph).unwrap();
	let child_b = seed.define_graph("child_b", GraphKind::Graph).unwrap();
	let transition = seed.define_graph("transition", GraphKind::Graph).unwrap();
	let meta = seed.define_graph("scope_meta", GraphKind::Metagraph).unwrap();
	let a = "module:a".parse::<RecordId>().unwrap();
	let b = "module:b".parse::<RecordId>().unwrap();
	let plain = "module:plain".parse::<RecordId>().unwrap();
	let meta_edge = "bridge:meta".parse::<RecordId>().unwrap();
	let plain_edge = "bridge:plain".parse::<RecordId>().unwrap();
	for record in [&a, &b, &plain, &meta_edge, &plain_edge] {
		seed.create_atom(record.clone(), PropertyMap::new()).unwrap();
	}
	let a_local = seed.attach_node(meta.id(), &a, None, Some(child_a.id())).unwrap();
	let b_local = seed.attach_node(meta.id(), &b, None, Some(child_b.id())).unwrap();
	seed.attach_node(meta.id(), &plain, None, None).unwrap();
	seed.attach_edge(
		meta.id(),
		&meta_edge,
		None,
		EdgeTopology::directed([a_local], [b_local]),
		None,
		Some(transition.id()),
	)
	.unwrap();
	seed.attach_edge(
		meta.id(),
		&plain_edge,
		None,
		EdgeTopology::directed([a_local], [b_local]),
		None,
		None,
	)
	.unwrap();
	seed.commit().unwrap();

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

#[test]
fn within_is_bounded_transitive_and_preserves_multiple_owners() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("module", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("document", AtomKind::Node, []).unwrap()).unwrap();
	let leaf_graph = seed.define_graph("within_leaf", GraphKind::Graph).unwrap();
	let middle_graph = seed.define_graph("within_middle", GraphKind::Metagraph).unwrap();
	let root_graph = seed.define_graph("within_root", GraphKind::Metagraph).unwrap();
	let leaf = "document:leaf".parse::<RecordId>().unwrap();
	let middle = "module:middle".parse::<RecordId>().unwrap();
	let back = "module:back".parse::<RecordId>().unwrap();
	let first = "module:first".parse::<RecordId>().unwrap();
	let second = "module:second".parse::<RecordId>().unwrap();
	for record in [&leaf, &middle, &back, &first, &second] {
		seed.create_atom(record.clone(), PropertyMap::new()).unwrap();
	}
	seed.attach_node(leaf_graph.id(), &leaf, None, None).unwrap();
	seed.attach_node(middle_graph.id(), &middle, None, Some(leaf_graph.id())).unwrap();
	seed.attach_node(middle_graph.id(), &back, None, Some(root_graph.id())).unwrap();
	seed.attach_node(root_graph.id(), &first, None, Some(middle_graph.id())).unwrap();
	seed.attach_node(root_graph.id(), &second, None, Some(middle_graph.id())).unwrap();
	seed.commit().unwrap();

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
		.unwrap();
	assert!(overflow[0].result.is_err());
	let zero = transaction
		.execute(
			"MATCH (document AS d) WITHIN*[0..1] <(module AS owner)> IN GRAPH within_root RETURN d",
			&Object::new(),
		)
		.unwrap();
	assert!(zero[0].result.is_err());
	drop(transaction);

	let mut snapshot = store
		.begin_with(TransactionOptions::new(IsolationLevel::Snapshot, TransactionAccess::ReadOnly));
	let query = "MATCH (document AS d) WITHIN*[2..2] <(module AS owner)> IN GRAPH within_root \
	             RETURN d, owner";
	assert_eq!(row_count(&mut snapshot, query), 2);
	let mut writer = store.begin();
	let third = "module:third".parse::<RecordId>().unwrap();
	writer.create_atom(third.clone(), PropertyMap::new()).unwrap();
	writer.attach_node(root_graph.id(), &third, None, Some(middle_graph.id())).unwrap();
	writer.commit().unwrap();
	assert_eq!(row_count(&mut snapshot, query), 2, "the statement snapshot must not rebind graphs");
	assert_eq!(row_count(&mut store.begin(), query), 3);
}

#[test]
fn cross_level_filter_accepts_only_the_selected_nested_branch() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("module", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("document", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("bridge", AtomKind::Edge, []).unwrap()).unwrap();
	let child = seed.define_graph("filter_child", GraphKind::Graph).unwrap();
	let meta = seed.define_graph("filter_meta", GraphKind::Metagraph).unwrap();
	let module = "module:root".parse::<RecordId>().unwrap();
	let nested_document = "document:nested".parse::<RecordId>().unwrap();
	let local_document = "document:local".parse::<RecordId>().unwrap();
	let nested_bridge = "bridge:nested".parse::<RecordId>().unwrap();
	let local_bridge = "bridge:local".parse::<RecordId>().unwrap();
	for record in [&module, &nested_document, &local_document, &nested_bridge, &local_bridge] {
		seed.create_atom(record.clone(), PropertyMap::new()).unwrap();
	}
	let child_document = seed.attach_node(child.id(), &nested_document, None, None).unwrap();
	let owner = seed.attach_node(meta.id(), &module, None, Some(child.id())).unwrap();
	let local = seed.attach_node(meta.id(), &local_document, None, None).unwrap();
	seed.attach_edge(
		meta.id(),
		&nested_bridge,
		None,
		EdgeTopology::directed([EdgeEndpoint::Local(owner)], [EdgeEndpoint::Nested(
			QualifiedAtomId::new([owner], child_document).unwrap(),
		)]),
		None,
		None,
	)
	.unwrap();
	seed.attach_edge(
		meta.id(),
		&local_bridge,
		None,
		EdgeTopology::directed([owner], [local]),
		None,
		None,
	)
	.unwrap();
	seed.commit().unwrap();

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
}

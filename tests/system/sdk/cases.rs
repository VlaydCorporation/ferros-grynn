use ferros_grynn::{
	datastore::{ClassField, ClassSchema},
	graph::{
		AtomKind,
		AtomRef,
		Dag,
		EdgeTopology,
		Frozen,
		Graph,
		GraphKind,
		GraphMut,
		GraphRead,
		Mutable,
		TraversalDirection,
		bfs,
		dijkstra,
		query_to_memory,
		topological_sort,
	},
	values::{Bytes, Number, Object, PropertyMap, Type, Value, Vector},
};

use crate::support::{create_person, people, record, value};

/// Verifies mutable sdk materialization writes only on explicit flush and
/// commit.
#[test]
fn mutable_sdk_materialization_writes_only_on_explicit_flush_and_commit() {
	let db = database();
	let (name, _) = people(&db);
	create_person(&db, "person:alice", "Base", 30);
	let mut write = db.begin();
	let reference = write.define_graph("people", GraphKind::Graph).unwrap();
	write.attach_node(reference.id(), &record("person:alice"), None, None).unwrap();
	write.commit().unwrap();
	let mut tx = db.begin();
	let view = reference.read(&tx).unwrap();
	let mut owned = query_to_memory::<Graph<Mutable>, _>(&view).unwrap();
	drop(view);
	let id = owned.atom_id(&AtomRef::new(AtomKind::Node, record("person:alice"))).unwrap().unwrap();
	GraphMut::node_properties_mut(&mut owned, id).unwrap().insert(name, "Flushed");
	assert_eq!(
		tx.effective_property(reference.id(), &record("person:alice"), name).unwrap(),
		Some(Value::from("Base"))
	);
	owned.flush(&mut tx).unwrap();
	assert_eq!(
		tx.effective_property(reference.id(), &record("person:alice"), name).unwrap(),
		Some(Value::from("Flushed"))
	);
	assert_eq!(
		db.begin().effective_property(reference.id(), &record("person:alice"), name).unwrap(),
		Some(Value::from("Base"))
	);
	tx.commit().unwrap();
	let read = db.begin();
	let view = reference.read(&read).unwrap();
	let mut discarded = query_to_memory::<Graph<Mutable>, _>(&view).unwrap();
	let id =
		discarded.atom_id(&AtomRef::new(AtomKind::Node, record("person:alice"))).unwrap().unwrap();
	GraphMut::node_properties_mut(&mut discarded, id).unwrap().insert(name, "Discarded");
	drop(discarded);
	drop(view);
	drop(read);
	db.assert_committed(|store| {
		assert_eq!(
			store
				.begin()
				.effective_property(reference.id(), &record("person:alice"), name)
				.unwrap(),
			Some(Value::from("Flushed"))
		);
		let atom = store.begin().atom(&record("person:alice")).unwrap().unwrap();
		assert_eq!(atom.properties().get(name), Some(&Value::from("Base")));
	});
}

/// Verifies atom snapshots own their payload after transaction and datastore
/// drop.
#[test]
fn atom_snapshots_own_their_payload_after_transaction_and_datastore_drop() {
	let db = database();
	let (name, _) = people(&db);
	create_person(&db, "person:alice", "Original", 30);
	let mut write = db.begin();
	let before = write.atom(&record("person:alice")).unwrap().unwrap();
	write.set_base_property(before.record_id(), name, Value::from("Private")).unwrap();
	let after = write.atom(before.record_id()).unwrap().unwrap();
	drop(write);
	drop(db);
	assert_eq!(before.properties().get(name), Some(&Value::from("Original")));
	assert_eq!(after.properties().get(name), Some(&Value::from("Private")));
	assert_eq!(before.record_id(), after.record_id());
}

/// Verifies sdk runtime values round trip through atoms queries and local
/// recovery.
#[test]
fn sdk_runtime_values_round_trip_through_atoms_queries_and_local_recovery() {
	let db = database();
	let mut schema = db.begin();
	schema
		.define_class(
			ClassSchema::new("document", AtomKind::Node, [ClassField::new("payload", Type::Any)])
				.unwrap(),
		)
		.unwrap();
	schema.commit().unwrap();
	let key = db.property_dictionary().get("payload").unwrap();
	let payload = Value::Object(Object::from_iter([
		("text", Value::from("Привет, граф 💾")),
		("array", Value::Array(vec![Value::Null, Value::from(true), Value::from(-42_i64)].into())),
		("bytes", Value::Bytes(Bytes::from([0_u8, 1, 255]))),
		("reference", Value::from(record("document:other"))),
		("vector", Value::Vector(Vector::F32(vec![1.25, -2.5, 0.0]))),
	]));
	let mut properties = PropertyMap::new();
	properties.insert(key, payload.clone());
	let mut write = db.begin();
	write.create_atom(record("document:payload"), properties).unwrap();
	write.commit().unwrap();
	db.assert_committed(|store| {
		assert_eq!(
			store.begin().atom(&record("document:payload")).unwrap().unwrap().properties().get(key),
			Some(&payload)
		);
		assert_eq!(
			value(store, "SELECT VALUE payload FROM document;", &Object::new()),
			Value::Array(vec![payload.clone()].into())
		);
	});
}

/// Verifies sdk none and null remain distinct query parameters.
#[test]
fn sdk_none_and_null_remain_distinct_query_parameters() {
	let db = database();
	let parameters = Object::from_iter([("absent", Value::None), ("null", Value::Null)]);
	assert_eq!(value(&db, "RETURN $absent;", &parameters), Value::None);
	assert_eq!(value(&db, "RETURN $null;", &parameters), Value::Null);
}

/// Verifies sdk traversals and owned materialization use stable graph
/// references.
#[test]
fn sdk_traversals_and_owned_materialization_use_stable_graph_references() {
	let db = database();
	let mut write = db.begin();
	write.define_class(ClassSchema::new("node", AtomKind::Node, []).unwrap()).unwrap();
	write.define_class(ClassSchema::new("edge", AtomKind::Edge, []).unwrap()).unwrap();
	let graph = write.define_graph("weighted", GraphKind::Dag).unwrap();
	let mut nodes = Vec::new();
	for name in ["a", "b", "c", "isolated"] {
		let id = record(&format!("node:{name}"));
		write.create_atom(id.clone(), PropertyMap::new()).unwrap();
		nodes.push(write.attach_node(graph.id(), &id, None, None).unwrap());
	}
	for (name, from, to, weight) in [("ab", 0, 1, 1), ("bc", 1, 2, 2), ("ac", 0, 2, 10)] {
		let id = record(&format!("edge:{name}"));
		write.create_atom(id.clone(), PropertyMap::new()).unwrap();
		write
			.attach_edge(
				graph.id(),
				&id,
				None,
				EdgeTopology::directed([nodes[from]], [nodes[to]]),
				Some(Number::from(weight)),
				None,
			)
			.unwrap();
	}
	write.commit().unwrap();
	db.assert_committed(|store| {
		let read = store.begin();
		let view = graph.read(&read).unwrap();
		let ids = ["a", "b", "c", "isolated"].map(|name| {
			view.atom_id(&AtomRef::new(AtomKind::Node, record(&format!("node:{name}"))))
				.unwrap()
				.unwrap()
		});
		assert_eq!(bfs(&view, ids[0], TraversalDirection::Forward, Some(0)).unwrap(), vec![ids[0]]);
		let mut reachable = bfs(&view, ids[0], TraversalDirection::Forward, None).unwrap();
		reachable.sort();
		let mut expected = ids[..3].to_vec();
		expected.sort();
		assert_eq!(reachable, expected);
		let paths = dijkstra(&view, ids[0], TraversalDirection::Forward).unwrap();
		assert_eq!(paths.distance_to(ids[2]), Some(Number::from(3_i64)));
		assert_eq!(paths.path_to(ids[2]), Some(vec![ids[0], ids[1], ids[2]]));
		assert_eq!(paths.distance_to(ids[3]), None);
		assert_eq!(
			dijkstra(&view, ids[2], TraversalDirection::Backward).unwrap().distance_to(ids[0]),
			Some(Number::from(3_i64))
		);
		let order = topological_sort(&view).unwrap();
		assert_eq!(order.len(), 4);
		let mut sorted = order.clone();
		sorted.sort();
		let mut all_nodes = ids.to_vec();
		all_nodes.sort();
		assert_eq!(sorted, all_nodes);
		assert!(
			order.iter().position(|id| *id == ids[0]) < order.iter().position(|id| *id == ids[1])
		);
		assert!(
			order.iter().position(|id| *id == ids[1]) < order.iter().position(|id| *id == ids[2])
		);
		let owned = query_to_memory::<Dag<Frozen>, _>(&view).unwrap();
		drop(view);
		drop(read);
		assert_eq!(owned.node_count(), 4);
		assert_eq!(owned.edge_count(), 3);
	});
}

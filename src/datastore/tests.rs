#![allow(
	clippy::expect_used,
	clippy::unwrap_used,
	reason = "tests fail immediately when compact setup or assertions return an unexpected error"
)]

use std::{ops::Bound, time::Duration};

use rust_decimal::Decimal;

use super::{consts::DEFAULT_MAX_WRITE_SET_BYTES, *};
use crate::values::runtime::{
	Array,
	Bytes,
	DateTime,
	Duration as ValueDuration,
	Geometry,
	Object,
	Position,
	Range,
	Set,
	Type,
	Ulid,
	Value,
	Vector,
	graph::{EdgeTopology, GraphMut, QualifiedAtomId, query_to_memory},
};

/// Unique database directory removed after the Windows integration test.
#[cfg(windows)]
struct LocalTestDirectory(std::path::PathBuf);

#[cfg(windows)]
impl LocalTestDirectory {
	/// Reserves a unique path without creating it before `create_local`.
	fn new() -> Self {
		Self(
			std::env::temp_dir()
				.join(format!("fgdb-{:016x}", Uuid::now_v7().as_inner().as_u128() as u64)),
		)
	}

	/// Returns the exact database path.
	fn path(&self) -> &Path {
		&self.0
	}
}

#[cfg(windows)]
impl Drop for LocalTestDirectory {
	fn drop(&mut self) {
		drop(std::fs::remove_dir_all(&self.0));
	}
}

fn graph_id(name: &str) -> RecordId {
	format!("graph:{name}").parse().expect("test graph ID must be valid")
}

fn seeded_store() -> (Datastore, GraphRef, AtomId, AtomId) {
	let store = Datastore::in_memory();
	let mut graph = Graph::<Mutable>::new();
	let left = graph.add_node(None, PropertyMap::new()).unwrap();
	let right = graph.add_node(None, PropertyMap::new()).unwrap();
	let mut tx = store.begin();
	let reference = tx.create_graph(graph_id("main"), graph.freeze().into()).unwrap();
	tx.commit().unwrap();
	(store, reference, left, right)
}

fn seeded_table() -> (Datastore, GraphRef, AtomId) {
	let store = Datastore::in_memory();
	let mut graph = Graph::<Mutable>::new();
	let name = store.intern_property("name").unwrap();
	let age = store.intern_property("age").unwrap();
	for (id, person_name, person_age) in
		[("carol", "Carol", 17_i64), ("alice", "Alice", 30), ("bob", "Bob", 20)]
	{
		let mut properties = PropertyMap::new();
		properties.insert(name, person_name);
		properties.insert(age, person_age);
		graph.add_node_with_id(format!("person:{id}").parse().unwrap(), None, properties).unwrap();
	}
	let mut unrelated = PropertyMap::new();
	unrelated.insert(name, "Mallory");
	unrelated.insert(age, 99_i64);
	graph.add_node_with_id("account:mallory".parse().unwrap(), None, unrelated).unwrap();
	let carol = graph
		.node_ids()
		.unwrap()
		.find_map(|id| {
			let id = id.unwrap();
			(graph.node(id).unwrap().record_id().to_string() == "person:carol").then_some(id)
		})
		.unwrap();
	let mut tx = store.begin();
	let reference = tx.create_graph(graph_id("people"), graph.freeze().into()).unwrap();
	tx.define_table(
		TableSchema::new("person", reference.clone(), TableKind::Normal, [
			TableField::new("name", Type::String),
			TableField::new("age", Type::Int),
		])
		.unwrap(),
	)
	.unwrap();
	tx.commit().unwrap();
	(store, reference, carol)
}

#[test]
fn named_graphs_validate_names_and_are_visible_to_the_creating_transaction() {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	let reference = transaction
		.create_named_graph("social", graph_id("internal"), Graph::<Mutable>::new().freeze().into())
		.unwrap();
	let snapshot = transaction.query_snapshot().unwrap();
	assert_eq!(transaction.query_named_graph(&snapshot, "social").unwrap(), reference);
	assert!(
		transaction
			.create_named_graph(
				"social",
				graph_id("duplicate"),
				Graph::<Mutable>::new().freeze().into(),
			)
			.is_err()
	);
	for invalid in ["", "nul\0name"] {
		assert!(
			transaction
				.create_named_graph(
					invalid,
					graph_id("invalid"),
					Graph::<Mutable>::new().freeze().into(),
				)
				.is_err()
		);
	}
	assert!(
		transaction
			.create_named_graph(
				"я".repeat(65),
				graph_id("too-long"),
				Graph::<Mutable>::new().freeze().into(),
			)
			.is_err()
	);
	transaction.remove_graph(&reference).unwrap();
	let snapshot = transaction.query_snapshot().unwrap();
	assert!(transaction.query_named_graph(&snapshot, "social").is_err());
}

#[test]
fn named_graph_names_are_database_wide_and_first_committer_wins() {
	let store = Datastore::in_memory();
	let mut first = store.begin();
	first
		.create_named_graph("shared", graph_id("first"), Graph::<Mutable>::new().freeze().into())
		.unwrap();
	let mut second = store.begin();
	second
		.create_named_graph("shared", graph_id("second"), Graph::<Mutable>::new().freeze().into())
		.unwrap();
	first.commit().unwrap();
	assert!(second.commit().is_err());

	let mut after_commit = store.begin();
	assert!(
		after_commit
			.create_named_graph(
				"shared",
				graph_id("third"),
				Graph::<Mutable>::new().freeze().into(),
			)
			.is_err()
	);
}

#[test]
fn create_graph_uses_the_record_value_as_its_default_name() {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	let reference = transaction
		.create_graph(graph_id("default-name"), Graph::<Mutable>::new().freeze().into())
		.unwrap();
	let snapshot = transaction.query_snapshot().unwrap();
	assert_eq!(transaction.query_named_graph(&snapshot, "default-name").unwrap(), reference);
}

/// The first public local slice survives close/recovery and remains visible
/// through both the Rust graph API and the read-only query adapter.
#[cfg(windows)]
#[test]
fn local_graph_table_and_edge_survive_reopen() {
	let directory = LocalTestDirectory::new();
	let options = LocalOptions {
		buffer_pool_pages: 16,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(directory.path(), options).unwrap();
	assert_eq!(store.options(), DatastoreOptions::Local(options));
	let name = store.intern_property("name").unwrap();
	let age = store.intern_property("age").unwrap();

	let mut graph = Graph::<Mutable>::new();
	let mut alice_properties = PropertyMap::new();
	alice_properties.insert(name, "Alice");
	alice_properties.insert(age, 30_i64);
	let alice = graph
		.add_node_with_id("person:alice".parse().unwrap(), Some("Person".into()), alice_properties)
		.unwrap();
	let mut bob_properties = PropertyMap::new();
	bob_properties.insert(name, "Bob");
	bob_properties.insert(age, 20_i64);
	let bob = graph
		.add_node_with_id("person:bob".parse().unwrap(), Some("Person".into()), bob_properties)
		.unwrap();
	graph
		.add_edge_with_id(
			"knows:alice-bob".parse().unwrap(),
			EdgeTopology::directed([alice], [bob]),
			Some("KNOWS".into()),
			PropertyMap::new(),
		)
		.unwrap();

	let mut transaction = store.begin();
	let reference = transaction.create_graph(graph_id("people"), graph.freeze().into()).unwrap();
	assert_eq!(reference.storage_mode(), GraphStorageMode::DiskBacked);
	transaction
		.define_table(
			TableSchema::new("person", reference, TableKind::Normal, [
				TableField::new("name", Type::String),
				TableField::new("age", Type::Int),
			])
			.unwrap()
			.with_index(TableIndex::new("person_age", TableIndexKind::Standard, ["age"]).unwrap())
			.unwrap()
			.with_index(
				TableIndex::new("person_name_age", TableIndexKind::Unique, ["name", "age"])
					.unwrap(),
			)
			.unwrap(),
		)
		.unwrap();
	transaction.commit().unwrap();

	let busy = match Datastore::open_local(directory.path(), options) {
		Ok(_unexpected) => panic!("a live local owner must keep db.lock exclusive"),
		Err(error) => error,
	};
	assert_eq!(busy.status().code().to_string(), "FG-51I03");
	drop(store);

	let reopened = Datastore::open_local(directory.path(), options).unwrap();
	let schema = reopened.table("person").unwrap();
	let transaction = reopened.begin();
	let view = schema.graph().read(&transaction).unwrap();
	assert_eq!(view.node_count().unwrap(), 2);
	assert_eq!(view.edge_count().unwrap(), 1);
	assert_eq!(
		view.node(alice).unwrap().unwrap().properties().unwrap().get(name),
		Some(&Value::from("Alice"))
	);
	drop(transaction);

	let results = reopened
		.execute("SELECT VALUE name FROM person WHERE age >= 18", &crate::values::Object::new())
		.unwrap();
	let Value::Array(rows) = results[0].result.as_ref().unwrap().value() else {
		panic!("SELECT must return an array");
	};
	assert_eq!(rows.as_slice(), &[Value::from("Alice"), Value::from("Bob")]);
	let composite = reopened
		.execute(
			"SELECT VALUE id FROM person WHERE name = 'Alice' AND age = 30",
			&crate::values::Object::new(),
		)
		.unwrap();
	let Value::Array(rows) = composite[0].result.as_ref().unwrap().value() else {
		panic!("composite-index SELECT must return an array");
	};
	assert_eq!(rows.as_slice(), &[Value::from("person:alice".parse::<RecordId>().unwrap())]);
	let point = reopened
		.execute("SELECT VALUE name FROM person:alice", &crate::values::Object::new())
		.unwrap();
	let Value::Array(rows) = point[0].result.as_ref().unwrap().value() else {
		panic!("point SELECT must return an array");
	};
	assert_eq!(rows.as_slice(), &[Value::from("Alice")]);

	let mut update = reopened.begin();
	if let GraphWriteView::Graph(graph) = update.edit_graph(schema.graph()).unwrap() {
		graph.node_properties_mut(alice).unwrap().insert(age, 31_i64);
	}
	update.commit().unwrap();
	drop(schema);
	drop(reopened);

	let reopened = Datastore::open_local(directory.path(), options).unwrap();
	let changed = reopened
		.execute("SELECT VALUE age FROM person:alice", &crate::values::Object::new())
		.unwrap();
	let Value::Array(rows) = changed[0].result.as_ref().unwrap().value() else {
		panic!("updated point SELECT must return an array");
	};
	assert_eq!(rows.as_slice(), &[Value::from(31_i64)]);
}

#[cfg(windows)]
#[test]
fn local_named_graph_resolves_to_the_same_reference_after_reopen() {
	let directory = LocalTestDirectory::new();
	let options = LocalOptions {
		buffer_pool_pages: 16,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(directory.path(), options).unwrap();
	let mut create = store.begin();
	let reference = create
		.create_named_graph(
			"Пользовательский граф",
			graph_id("durable-name"),
			Graph::<Mutable>::new().freeze().into(),
		)
		.unwrap();
	create.commit().unwrap();
	drop(store);

	let reopened = Datastore::open_local(directory.path(), options).unwrap();
	let transaction = reopened.begin();
	let snapshot = transaction.query_snapshot().unwrap();
	let resolved = transaction.query_named_graph(&snapshot, "Пользовательский граф").unwrap();
	assert_eq!(resolved, reference);
}

/// The public NativeAsync selector drives IoRing through WAL commit,
/// checkpoint backfill, close, and reopen without changing query semantics.
#[cfg(all(windows, feature = "native-async-io"))]
#[test]
fn native_async_local_checkpoint_survives_reopen() {
	let directory = LocalTestDirectory::new();
	let options = LocalOptions {
		buffer_pool_pages: 32,
		io_mode: IoMode::NativeAsync,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(directory.path(), options).unwrap();
	let mut graph = Graph::<Mutable>::new();
	for index in 0..16 {
		graph
			.add_node_with_id(format!("native:{index}").parse().unwrap(), None, PropertyMap::new())
			.unwrap();
	}
	let mut transaction = store.begin();
	let graph = transaction.create_graph(graph_id("native"), graph.freeze().into()).unwrap();
	transaction.commit().unwrap();
	store.checkpoint(CheckpointMode::Restart).unwrap();
	drop(store);

	let reopened = Datastore::open_local(directory.path(), options).unwrap();
	let transaction = reopened.begin();
	assert_eq!(graph.read(&transaction).unwrap().node_count().unwrap(), 16);
}

/// Nested graphs, transition graphs and cross-level incidence remain
/// readable after recovery and through explicit hybrid materialization.
#[cfg(windows)]
#[test]
fn local_metagraph_round_trips_nested_content_and_cross_level_incidence() {
	let directory = LocalTestDirectory::new();
	let options = LocalOptions {
		buffer_pool_pages: 16,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(directory.path(), options).unwrap();
	let mut inner = Graph::<Mutable>::new();
	let inner_node =
		inner.add_node_with_id("inner:node".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let inner_second =
		inner.add_node_with_id("inner:second".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let mut transition = Graph::<Mutable>::new();
	let transition_node = transition
		.add_node_with_id("transition:node".parse().unwrap(), None, PropertyMap::new())
		.unwrap();
	let mut graph = Metagraph::<Mutable>::new();
	let meta = graph.add_metavertex(inner.freeze(), None, PropertyMap::new()).unwrap();
	let local = graph.add_node(None, PropertyMap::new()).unwrap();
	let nested = QualifiedAtomId::new([meta], inner_node).unwrap();
	let nested_second = QualifiedAtomId::new([meta], inner_second).unwrap();
	let cross_topology = EdgeTopology::directed(
		[
			EdgeEndpoint::Nested(nested.clone()),
			EdgeEndpoint::Nested(nested_second),
			EdgeEndpoint::Local(meta),
		],
		[EdgeEndpoint::Local(local)],
	);
	let cross = graph.add_edge(cross_topology.clone(), None, PropertyMap::new()).unwrap();
	let connector =
		graph.add_edge(EdgeTopology::directed([cross], [local]), None, PropertyMap::new()).unwrap();
	let metaedge = graph
		.add_metaedge(
			EdgeTopology::directed([meta], [local]),
			transition.freeze(),
			None,
			PropertyMap::new(),
		)
		.unwrap();
	let mut create = store.begin();
	let reference = create.create_graph(graph_id("meta"), graph.freeze().into()).unwrap();
	create.commit().unwrap();
	drop(store);

	let store = Datastore::open_local(directory.path(), options).unwrap();
	let read = store.begin();
	let view = reference.read(&read).unwrap();
	let meta_node = view.node(meta).unwrap().unwrap();
	let inner = meta_node.inner_graph().unwrap().unwrap();
	assert!(inner.node(inner_node).is_some());
	assert!(inner.node(inner_second).is_some());
	assert_eq!(view.edge(cross).unwrap().unwrap().topology().unwrap().as_ref(), &cross_topology);
	assert_eq!(
		view.edge(connector).unwrap().unwrap().topology().unwrap().local_binary(),
		Some((cross, local, EdgeDirection::Directed))
	);
	let meta_edge = view.edge(metaedge).unwrap().unwrap();
	let transition = meta_edge.transition_graph().unwrap().unwrap();
	assert!(transition.node(transition_node).is_some());

	let hybrid = query_to_memory::<Graph<Frozen>, _>(&view).unwrap();
	assert_eq!(hybrid.node_count(), 2);
	assert_eq!(hybrid.edge_count(), 3);
	assert!(hybrid.node_ids().unwrap().any(|id| {
		let id = id.unwrap();
		hybrid.node(id).unwrap().inner_graph().is_some()
	}));
	assert!(hybrid.edge_ids().unwrap().any(|id| {
		let id = id.unwrap();
		hybrid.edge(id).unwrap().transition_graph().is_some()
	}));
}

/// One WAL commit publishes two graphs atomically, and a concurrent update
/// to another atom in the same hot page merges against that durable image.
#[cfg(windows)]
#[test]
fn local_multi_graph_commit_and_same_page_merge_survive_reopen() {
	let directory = LocalTestDirectory::new();
	let options = LocalOptions {
		buffer_pool_pages: 16,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(directory.path(), options).unwrap();
	let value = store.intern_property("value").unwrap();
	let mut first_graph = Graph::<Mutable>::new();
	let left = first_graph.add_node(None, PropertyMap::new()).unwrap();
	let right = first_graph.add_node(None, PropertyMap::new()).unwrap();
	let mut second_graph = Graph::<Mutable>::new();
	let other = second_graph.add_node(None, PropertyMap::new()).unwrap();
	let mut create = store.begin();
	let first_ref = create.create_graph(graph_id("first"), first_graph.freeze().into()).unwrap();
	let second_ref = create.create_graph(graph_id("second"), second_graph.freeze().into()).unwrap();
	create.commit().unwrap();
	assert!(matches!(
		&store.current_root().graphs.get(first_ref.id()).unwrap().graph,
		StoredGraphValue::Local(_)
	));
	assert!(matches!(
		&store.current_root().graphs.get(second_ref.id()).unwrap().graph,
		StoredGraphValue::Local(_)
	));

	let transaction_options = TransactionOptions {
		isolation: IsolationLevel::Snapshot,
		access: TransactionAccess::ReadWrite,
	};
	let mut multi_graph = store.begin_with(transaction_options);
	let mut same_page = store.begin_with(transaction_options);
	if let GraphWriteView::Graph(graph) = multi_graph.edit_graph(&first_ref).unwrap() {
		graph.node_properties_mut(left).unwrap().insert(value, 1_i64);
	}
	if let GraphWriteView::Graph(graph) = multi_graph.edit_graph(&second_ref).unwrap() {
		graph.node_properties_mut(other).unwrap().insert(value, 2_i64);
	}
	if let GraphWriteView::Graph(graph) = same_page.edit_graph(&first_ref).unwrap() {
		graph.node_properties_mut(right).unwrap().insert(value, 3_i64);
	}
	multi_graph.commit().unwrap();
	assert!(matches!(
		&store.current_root().graphs.get(first_ref.id()).unwrap().graph,
		StoredGraphValue::Local(_)
	));
	same_page.commit().unwrap();
	drop(store);

	let store = Datastore::open_local(directory.path(), options).unwrap();
	let read = store.begin();
	let first = first_ref.read(&read).unwrap();
	assert_eq!(
		first.node(left).unwrap().unwrap().properties().unwrap().get(value),
		Some(&Value::from(1_i64))
	);
	assert_eq!(
		first.node(right).unwrap().unwrap().properties().unwrap().get(value),
		Some(&Value::from(3_i64))
	);
	let second = second_ref.read(&read).unwrap();
	assert_eq!(
		second.node(other).unwrap().unwrap().properties().unwrap().get(value),
		Some(&Value::from(2_i64))
	);
}

/// A failed unique-index build publishes neither its graph nor schema,
/// including after reopening the database.
#[cfg(windows)]
#[test]
fn local_unique_index_violation_rolls_back_publication() {
	let directory = LocalTestDirectory::new();
	let options = LocalOptions {
		buffer_pool_pages: 16,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(directory.path(), options).unwrap();
	let email = store.intern_property("email").unwrap();
	let mut graph = Graph::<Mutable>::new();
	for id in ["first", "second"] {
		let mut properties = PropertyMap::new();
		properties.insert(email, "same@example.test");
		graph.add_node_with_id(format!("person:{id}").parse().unwrap(), None, properties).unwrap();
	}
	let mut create = store.begin();
	let reference = create.create_graph(graph_id("duplicates"), graph.freeze().into()).unwrap();
	create
		.define_table(
			TableSchema::new("person", reference.clone(), TableKind::Normal, [TableField::new(
				"email",
				Type::String,
			)])
			.unwrap()
			.with_index(TableIndex::new("person_email", TableIndexKind::Unique, ["email"]).unwrap())
			.unwrap(),
		)
		.unwrap();
	assert!(create.commit().is_err());
	assert!(store.table("person").is_none());
	assert!(reference.read(&store.begin()).is_err());
	drop(store);

	let store = Datastore::open_local(directory.path(), options).unwrap();
	assert!(store.table("person").is_none());
	assert!(reference.read(&store.begin()).is_err());
}

/// Stable AtomRef point mutations preserve incidence and properties across
/// close/recovery without exposing transaction-local AtomId handles.
#[cfg(windows)]
#[test]
fn local_point_mutations_survive_reopen() {
	let directory = LocalTestDirectory::new();
	let options = LocalOptions {
		buffer_pool_pages: 16,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(directory.path(), options).unwrap();
	let score = store.intern_property("score").unwrap();
	let transient = store.intern_property("transient").unwrap();
	let mut create = store.begin();
	let graph = create
		.create_graph(graph_id("mutations"), Graph::<Mutable>::new().freeze().into())
		.unwrap();
	create.commit().unwrap();
	drop(store);

	let store = Datastore::open_local(directory.path(), options).unwrap();
	let mut transaction = store.begin();
	let mut mutation = transaction.mutate_graph(&graph).unwrap();
	let alpha = mutation
		.create_node("item:alpha".parse().unwrap(), Some("Item".into()), PropertyMap::new())
		.unwrap();
	let beta =
		mutation.create_node("item:beta".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let gamma =
		mutation.create_node("item:gamma".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let edge = mutation
		.create_edge(
			"link:one".parse().unwrap(),
			&AtomRefTopology::Directed {
				source: vec![alpha.clone()],
				target: vec![beta.clone()],
			},
			None,
			PropertyMap::new(),
		)
		.unwrap();
	mutation.set_property(&alpha, score, Value::from(7_i64)).unwrap();
	assert_eq!(mutation.set_label(&alpha, Some("Changed".into())).unwrap(), Some("Item".into()));
	mutation.set_property(&alpha, transient, Value::from(true)).unwrap();
	assert_eq!(mutation.remove_property(&alpha, transient).unwrap(), Some(Value::from(true)));
	mutation
		.replace_incidence(&edge, &AtomRefTopology::Directed {
			source: vec![alpha.clone()],
			target: vec![gamma.clone()],
		})
		.unwrap();
	mutation.remove_atom(&beta).unwrap();
	drop(mutation);
	transaction.commit().unwrap();
	drop(store);

	let store = Datastore::open_local(directory.path(), options).unwrap();
	let transaction = store.begin();
	let view = graph.read(&transaction).unwrap();
	let alpha_id = view.atom_id(&alpha).unwrap().unwrap();
	let gamma_id = view.atom_id(&gamma).unwrap().unwrap();
	let edge_id = view.atom_id(&edge).unwrap().unwrap();
	assert!(view.atom_id(&beta).unwrap().is_none());
	assert_eq!(
		view.node(alpha_id).unwrap().unwrap().properties().unwrap().get(score),
		Some(&Value::from(7_i64))
	);
	assert_eq!(
		view.node(alpha_id).unwrap().unwrap().label_text().unwrap().as_deref(),
		Some(&Text::from("Changed"))
	);
	assert_eq!(
		view.edge(edge_id).unwrap().unwrap().topology().unwrap().local_binary(),
		Some((alpha_id, gamma_id, EdgeDirection::Directed))
	);
}

/// Appending nodes through the backend-neutral mutation view retains only the
/// transaction delta until commit instead of materializing the local graph.
#[cfg(windows)]
#[test]
fn local_node_append_stays_delta_backed_until_commit() {
	let directory = LocalTestDirectory::new();
	let options = LocalOptions {
		buffer_pool_pages: 16,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(directory.path(), options).unwrap();
	let mut create = store.begin();
	let graph = create
		.create_graph(graph_id("delta-append"), Graph::<Mutable>::new().freeze().into())
		.unwrap();
	create.commit().unwrap();

	let payload_key = store.intern_property("payload").unwrap();
	let payload = Bytes::from(vec![0x5a; 20_000]);
	let composite: RecordId = r#"item:[7,{"region":"eu"}]"#.parse().unwrap();
	let scalar: RecordId = "item:second".parse().unwrap();
	let mut transaction = store.begin();
	{
		let mut mutation = transaction.mutate_graph(&graph).unwrap();
		let mut properties = PropertyMap::new();
		properties.insert(payload_key, payload.clone());
		mutation.create_node(composite.clone(), Some("Composite".into()), properties).unwrap();
		mutation.create_node(scalar.clone(), None, PropertyMap::new()).unwrap();
	}
	let pending = transaction.graphs.get(graph.id()).unwrap();
	assert!(pending.is_delta_backed());
	transaction.commit().unwrap();
	drop(store);

	let reopened = Datastore::open_local(directory.path(), options).unwrap();
	let read = reopened.begin();
	let view = graph.read(&read).unwrap();
	let composite = view.atom_id(&AtomRef::new(AtomKind::Node, composite)).unwrap().unwrap();
	let composite = view.node(composite).unwrap().unwrap();
	assert_eq!(composite.label_text().unwrap().as_deref(), Some(&Text::from("Composite")));
	assert_eq!(composite.properties().unwrap().get(payload_key), Some(&Value::from(payload)));
	assert!(view.atom_id(&AtomRef::new(AtomKind::Node, scalar)).unwrap().is_some());
}

/// The public local path reads large property chains and adjacency
/// overflow, while an aborted materialized mutation remains
/// transaction-private.
#[cfg(windows)]
#[test]
fn local_large_properties_overflow_and_abort_round_trip() {
	let directory = LocalTestDirectory::new();
	let options = LocalOptions {
		buffer_pool_pages: 16,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(directory.path(), options).unwrap();
	let payload_key = store.intern_property("payload").unwrap();
	let payload = Text::from("x".repeat(10_000));
	let mut graph = Graph::<Mutable>::new();
	let mut source = Vec::new();
	let mut target = Vec::new();
	for index in 0..6 {
		let mut properties = PropertyMap::new();
		if index == 0 {
			properties.insert(payload_key, payload.clone());
		}
		source.push(
			graph
				.add_node_with_id(format!("item:source-{index}").parse().unwrap(), None, properties)
				.unwrap(),
		);
		target.push(
			graph
				.add_node_with_id(
					format!("item:target-{index}").parse().unwrap(),
					None,
					PropertyMap::new(),
				)
				.unwrap(),
		);
	}
	let edge = graph
		.add_edge_with_id(
			"link:wide".parse().unwrap(),
			EdgeTopology::directed(source.clone(), target.clone()),
			None,
			PropertyMap::new(),
		)
		.unwrap();
	let mut create = store.begin();
	let reference = create.create_graph(graph_id("wide"), graph.freeze().into()).unwrap();
	create.commit().unwrap();
	drop(store);

	let store = Datastore::open_local(directory.path(), options).unwrap();
	let read = store.begin();
	let view = reference.read(&read).unwrap();
	let node = view.node(source[0]).unwrap().unwrap();
	assert_eq!(node.properties().unwrap().get(payload_key), Some(&Value::from(payload.clone())));
	let wide_edge = view.edge(edge).unwrap().unwrap();
	let topology = wide_edge.topology().unwrap();
	let EdgeTopology::Directed {
		source: decoded_source,
		target: decoded_target,
	} = topology.as_ref()
	else {
		panic!("wide directed edge changed topology");
	};
	assert_eq!(decoded_source.len(), source.len());
	assert_eq!(decoded_target.len(), target.len());
	drop(topology);
	drop(wide_edge);
	drop(node);
	drop(view);
	drop(read);

	let source_ref = AtomRef::new(AtomKind::Node, "item:source-0".parse().unwrap());
	let mut aborted = store.begin();
	aborted
		.mutate_graph(&reference)
		.unwrap()
		.set_property(&source_ref, payload_key, Value::from("discarded"))
		.unwrap();
	aborted.abort();
	let read = store.begin();
	let view = reference.read(&read).unwrap();
	assert_eq!(
		view.node(source[0]).unwrap().unwrap().properties().unwrap().get(payload_key),
		Some(&Value::from(payload))
	);
}

/// Every runtime Value variant admitted by the format-v1 property codec
/// survives the complete local commit, recovery and reference-resolution
/// path.
#[cfg(windows)]
#[test]
fn local_persistable_value_tags_round_trip() {
	let directory = LocalTestDirectory::new();
	let options = LocalOptions {
		buffer_pool_pages: 16,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(directory.path(), options).unwrap();
	let all = store.intern_property("all").unwrap();
	let anchor_record: RecordId = "value:anchor".parse().unwrap();
	let anchor_ref = AtomRef::new(AtomKind::Node, anchor_record.clone());
	let point = Position::new([10.0, 20.0]).unwrap();
	let second = Position::new([-10.0, -20.0]).unwrap();
	let mut values = Object::new();
	values.insert("none", Value::None);
	values.insert("null", Value::Null);
	values.insert("false", false);
	values.insert("true", true);
	values.insert("int", i64::MIN);
	values.insert("float", 1.5_f64);
	values.insert("decimal", "-123.45".parse::<Decimal>().unwrap());
	values.insert("string", "hello");
	values.insert("bytes", Bytes::from([0_u8, 255]));
	values.insert(
		"datetime",
		"1969-12-31T23:59:59.500000000+00:00[UTC]".parse::<DateTime>().unwrap(),
	);
	values.insert("duration", "P1DT2H".parse::<ValueDuration>().unwrap());
	values.insert("uuid", Uuid::new_v4());
	values.insert("ulid", Ulid::new());
	values.insert("record", "person:stored".parse::<RecordId>().unwrap());
	values.insert("array", Array::new(vec![Value::from(1_i64), Value::from("x")]));
	values.insert("set", Set::from(vec![1_i64, 2]));
	values.insert("object", Object::from_iter([("nested", Value::from("value"))]));
	values.insert(
		"range",
		Range::new(Bound::Included(1_i64.into()), Bound::Excluded(3_i64.into())).unwrap(),
	);
	values.insert("point", Geometry::Point(point.clone()));
	values.insert("line", Geometry::LineString(vec![point.clone(), second.clone()]));
	values.insert("polygon", Geometry::Polygon(vec![vec![point.clone(), second.clone()]]));
	values.insert("multipoint", Geometry::MultiPoint(vec![point.clone(), second.clone()]));
	values
		.insert("multiline", Geometry::MultiLineString(vec![vec![point.clone(), second.clone()]]));
	values.insert(
		"multipolygon",
		Geometry::MultiPolygon(vec![vec![vec![point.clone(), second.clone()]]]),
	);
	values.insert("geometry_collection", Geometry::Collection(vec![Geometry::Point(point)]));
	values.insert("vector_f64", Vector::F64(vec![1.0, 2.0]));
	values.insert("vector_f32", Vector::F32(vec![1.0, 2.0]));
	values.insert("vector_i64", Vector::I64(vec![1, 2]));
	values.insert("vector_i32", Vector::I32(vec![1, 2]));
	values.insert("vector_i16", Vector::I16(vec![1, 2]));
	values.insert("atom", anchor_ref);

	let mut properties = PropertyMap::new();
	properties.insert(all, Value::Object(values.clone()));
	let mut graph = Graph::<Mutable>::new();
	let anchor = graph.add_node_with_id(anchor_record, None, properties).unwrap();
	let mut create = store.begin();
	let reference = create.create_graph(graph_id("values"), graph.freeze().into()).unwrap();
	create.commit().unwrap();
	drop(store);

	let store = Datastore::open_local(directory.path(), options).unwrap();
	let read = store.begin();
	let view = reference.read(&read).unwrap();
	assert_eq!(
		view.node(anchor).unwrap().unwrap().properties().unwrap().get(all),
		Some(&Value::Object(values))
	);
}

/// A disk tombstone preserves the allocator generation across reopen, so a
/// reused slot cannot make an old transaction-local handle valid again.
#[cfg(windows)]
#[test]
fn local_reopen_preserves_reused_slot_generation() {
	let directory = LocalTestDirectory::new();
	let options = LocalOptions {
		buffer_pool_pages: 16,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(directory.path(), options).unwrap();
	let mut graph = Graph::<Mutable>::new();
	let stale =
		graph.add_node_with_id("item:stale".parse().unwrap(), None, PropertyMap::new()).unwrap();
	graph.add_node_with_id("item:kept".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let mut create = store.begin();
	let reference = create.create_graph(graph_id("slots"), graph.freeze().into()).unwrap();
	create.commit().unwrap();
	drop(store);

	let reopened = Datastore::open_local(directory.path(), options).unwrap();
	let mut remove = reopened.begin();
	if let GraphWriteView::Graph(graph) = remove.edit_graph(&reference).unwrap() {
		graph.remove_atom(stale).unwrap();
	}
	remove.commit().unwrap();
	drop(reopened);

	let reopened = Datastore::open_local(directory.path(), options).unwrap();
	let mut reuse = reopened.begin();
	let replacement = match reuse.edit_graph(&reference).unwrap() {
		GraphWriteView::Graph(graph) => graph
			.add_node_with_id("item:replacement".parse().unwrap(), None, PropertyMap::new())
			.unwrap(),
		_other => panic!("general graph must keep its family"),
	};
	assert_eq!(replacement.slot(), stale.slot());
	assert_eq!(replacement.generation(), stale.generation() + 1);
	reuse.commit().unwrap();
	drop(reopened);

	let reopened = Datastore::open_local(directory.path(), options).unwrap();
	let generation_count = std::fs::read_dir(directory.path().join("graphs"))
		.unwrap()
		.flat_map(|graph| std::fs::read_dir(graph.unwrap().path()).unwrap())
		.count();
	assert_eq!(generation_count, 1);
	let read = reopened.begin();
	let view = reference.read(&read).unwrap();
	assert!(view.node(stale).unwrap().is_none());
	assert!(view.node(replacement).unwrap().is_some());
}

/// Passive checkpoint preserves a reader snapshot while Restart invalidates
/// it and makes the complete durable tail recoverable from home files.
#[cfg(windows)]
#[test]
fn local_restart_checkpoint_cancels_old_snapshot() {
	let directory = LocalTestDirectory::new();
	let options = LocalOptions {
		buffer_pool_pages: 16,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(directory.path(), options).unwrap();
	let value = store.intern_property("value").unwrap();
	let mut properties = PropertyMap::new();
	properties.insert(value, 1_i64);
	let mut graph = Graph::<Mutable>::new();
	let node = graph.add_node_with_id("item:one".parse().unwrap(), None, properties).unwrap();
	let mut create = store.begin();
	let reference = create.create_graph(graph_id("checkpoint"), graph.freeze().into()).unwrap();
	create.commit().unwrap();
	drop(store);

	let store = Datastore::open_local(directory.path(), options).unwrap();
	let old = store.begin();
	let old_view = reference.read(&old).unwrap();
	let mut writer = store.begin();
	if let GraphWriteView::Graph(graph) = writer.edit_graph(&reference).unwrap() {
		graph.node_properties_mut(node).unwrap().insert(value, 2_i64);
	}
	writer.commit().unwrap();

	store.checkpoint(CheckpointMode::Passive).unwrap();
	assert_eq!(
		old_view.node(node).unwrap().unwrap().properties().unwrap().get(value),
		Some(&Value::from(1_i64))
	);
	store.checkpoint(CheckpointMode::Restart).unwrap();
	assert_eq!(old_view.node_count().unwrap_err().status().code().to_string(), "FG-25F02");
	drop(old_view);
	drop(old);
	drop(store);

	let store = Datastore::open_local(directory.path(), options).unwrap();
	let fresh = store.begin();
	let view = reference.read(&fresh).unwrap();
	assert_eq!(
		view.node(node).unwrap().unwrap().properties().unwrap().get(value),
		Some(&Value::from(2_i64))
	);
}

#[test]
fn snapshots_use_commit_time_not_begin_time() {
	let store = Datastore::in_memory();
	let old = store.begin();
	let mut late = store.begin();
	let reference =
		late.create_graph(graph_id("late"), Graph::<Mutable>::new().freeze().into()).unwrap();
	late.commit().unwrap();

	assert!(reference.read(&old).is_err());
	assert!(reference.read(&store.begin()).is_ok());
}

#[test]
fn atom_versions_keep_reused_slot_generations_distinct() {
	let old = AtomId::from_parts(AtomKind::Node, 7, 0).unwrap();
	let reused = AtomId::from_parts(AtomKind::Node, 7, 1).unwrap();
	let mut versions = AtomVersions::default();
	versions.insert(old, 3);
	versions.insert(reused, 5);
	assert_eq!(versions.get(old), 3);
	assert_eq!(versions.get(reused), 5);
}

#[test]
fn local_checkpoint_policy_validates_percent_and_rounds_up() {
	let validated = validate_local_options(LocalOptions {
		buffer_pool_pages: 10,
		checkpoint: CheckpointPolicy {
			unbackfilled_page_percent: 25,
			interval: Duration::ZERO,
		},
		..LocalOptions::default()
	})
	.unwrap();
	assert_eq!(validated.checkpoint_page_threshold, 3);
	for percent in [0, 101] {
		assert!(
			validate_local_options(LocalOptions {
				checkpoint: CheckpointPolicy {
					unbackfilled_page_percent: percent,
					..CheckpointPolicy::default()
				},
				..LocalOptions::default()
			})
			.is_err()
		);
	}
}

#[test]
fn read_committed_query_uses_one_statement_snapshot() {
	let (store, reference, carol) = seeded_table();
	let reader = store.begin_with(TransactionOptions {
		isolation: IsolationLevel::ReadCommitted,
		access: TransactionAccess::ReadOnly,
	});
	let statement = reader.query_snapshot().unwrap();

	let mut writer = store.begin();
	if let GraphWriteView::Graph(graph) = writer.edit_graph(&reference).unwrap() {
		graph.node_properties_mut(carol).unwrap().insert(1_u32.into(), 21_i64);
	}
	writer.commit().unwrap();

	let old_rows = reader.query_rows(&statement, "person", &[]).unwrap();
	let old_carol =
		old_rows.iter().find(|row| row.record_id.to_string() == "person:carol").unwrap();
	assert_eq!(old_carol.values[2], Value::from(17_i64));

	let fresh = reader.query_snapshot().unwrap();
	let fresh_rows = reader.query_rows(&fresh, "person", &[]).unwrap();
	let fresh_carol =
		fresh_rows.iter().find(|row| row.record_id.to_string() == "person:carol").unwrap();
	assert_eq!(fresh_carol.values[2], Value::from(21_i64));
}

#[test]
fn graph_removal_preserves_table_catalog_integrity() {
	let (store, reference, _) = seeded_table();
	let mut blocked = store.begin();
	blocked.remove_graph(&reference).unwrap();
	assert!(blocked.commit().is_err());

	let mut removal = store.begin();
	removal.remove_table("person").unwrap();
	removal.remove_graph(&reference).unwrap();
	removal.commit().unwrap();
	assert!(store.table("person").is_none());
	assert!(reference.read(&store.begin()).is_err());
}

#[test]
fn disjoint_atom_writes_merge() {
	let (store, reference, left, right) = seeded_store();
	let mut first = store.begin_with(TransactionOptions {
		isolation: IsolationLevel::Snapshot,
		access: TransactionAccess::ReadWrite,
	});
	let mut second = store.begin_with(first.options());
	let key = PropertyKeyId::new(0);
	if let GraphWriteView::Graph(graph) = first.edit_graph(&reference).unwrap() {
		graph.node_properties_mut(left).unwrap().insert(key, Value::from(1_i64));
	}
	if let GraphWriteView::Graph(graph) = second.edit_graph(&reference).unwrap() {
		graph.node_properties_mut(right).unwrap().insert(key, Value::from(2_i64));
	}
	first.commit().unwrap();
	second.commit().unwrap();

	let tx = store.begin();
	let view = reference.read(&tx).unwrap();
	assert_eq!(
		view.node(left).unwrap().unwrap().properties().unwrap().get(key),
		Some(&Value::from(1_i64))
	);
	assert_eq!(
		view.node(right).unwrap().unwrap().properties().unwrap().get(key),
		Some(&Value::from(2_i64))
	);
}

#[test]
fn concurrent_writes_to_the_same_atom_fail_first_committer_wins() {
	let (store, reference, left, _) = seeded_store();
	let options = TransactionOptions {
		isolation: IsolationLevel::Snapshot,
		access: TransactionAccess::ReadWrite,
	};
	let mut first = store.begin_with(options);
	let mut second = store.begin_with(options);
	if let GraphWriteView::Graph(graph) = first.edit_graph(&reference).unwrap() {
		graph.node_properties_mut(left).unwrap().insert(0_u32.into(), 1_i64);
	}
	if let GraphWriteView::Graph(graph) = second.edit_graph(&reference).unwrap() {
		graph.node_properties_mut(left).unwrap().insert(0_u32.into(), 2_i64);
	}
	first.commit().unwrap();
	assert_eq!(second.commit().unwrap_err().status().code().to_string(), "FG-40001");
}

#[test]
fn serializable_reader_aborts_after_concurrent_change() {
	let (store, reference, left, _) = seeded_store();
	let reader = store.begin();
	assert!(reference.read(&reader).unwrap().node(left).unwrap().is_some());

	let mut writer = store.begin();
	if let GraphWriteView::Graph(graph) = writer.edit_graph(&reference).unwrap() {
		graph.node_properties_mut(left).unwrap().insert(0_u32.into(), 1_i64);
	}
	writer.commit().unwrap();

	let error = reader.commit().unwrap_err();
	assert_eq!(error.status().code().to_string(), "FG-40001");
}

#[test]
fn read_only_snapshot_is_safe_from_serialization_aborts() {
	let (store, reference, left, _) = seeded_store();
	let reader = store.begin_with(TransactionOptions {
		isolation: IsolationLevel::Serializable,
		access: TransactionAccess::ReadOnly,
	});
	assert!(reference.read(&reader).unwrap().node(left).unwrap().is_some());
	let mut writer = store.begin();
	if let GraphWriteView::Graph(graph) = writer.edit_graph(&reference).unwrap() {
		graph.node_properties_mut(left).unwrap().insert(0_u32.into(), 1_i64);
	}
	writer.commit().unwrap();
	reader.commit().unwrap();
}

#[test]
fn select_uses_explicit_schema_and_record_id_order() {
	let (store, _, _) = seeded_table();
	let results = store
		.execute("SELECT VALUE name FROM person WHERE age >= 18", &crate::values::Object::new())
		.unwrap();
	let Value::Array(rows) = results[0].result.as_ref().unwrap().value() else {
		panic!("SELECT must return an array");
	};
	assert_eq!(rows.as_slice(), &[Value::from("Alice"), Value::from("Bob")]);
}

#[test]
fn predicate_validation_ignores_non_matching_changes_but_catches_phantoms() {
	let (store, reference, carol) = seeded_table();
	let reader = store.begin();
	reader
		.execute("SELECT VALUE name FROM person WHERE age >= 18", &crate::values::Object::new())
		.unwrap();
	let mut writer = store.begin();
	if let GraphWriteView::Graph(graph) = writer.edit_graph(&reference).unwrap() {
		graph.node_properties_mut(carol).unwrap().insert(1_u32.into(), 17_i64);
	}
	writer.commit().unwrap();
	reader.commit().unwrap();

	let reader = store.begin();
	reader
		.execute("SELECT VALUE name FROM person WHERE age >= 18", &crate::values::Object::new())
		.unwrap();
	let mut writer = store.begin();
	if let GraphWriteView::Graph(graph) = writer.edit_graph(&reference).unwrap() {
		graph.node_properties_mut(carol).unwrap().insert(1_u32.into(), 21_i64);
	}
	writer.commit().unwrap();
	assert_eq!(reader.commit().unwrap_err().status().code().to_string(), "FG-40001");
}

#[test]
fn predicate_validation_includes_own_uncommitted_writes() {
	let (store, reference, carol) = seeded_table();
	let mut transaction = store.begin();
	if let GraphWriteView::Graph(graph) = transaction.edit_graph(&reference).unwrap() {
		graph.node_properties_mut(carol).unwrap().insert(1_u32.into(), 21_i64);
	}
	transaction
		.execute("SELECT VALUE name FROM person WHERE age >= 18", &crate::values::Object::new())
		.unwrap();
	transaction.commit().unwrap();
}

#[test]
fn configured_transaction_limits_have_domain_statuses() {
	let store = Datastore::in_memory_with(InMemoryOptions {
		max_snapshot_age: Duration::from_secs(300),
		max_write_set_bytes: 0,
	});
	let mut transaction = store.begin();
	transaction.create_graph(graph_id("limited"), Graph::<Mutable>::new().freeze().into()).unwrap();
	assert_eq!(transaction.commit().unwrap_err().status().code().to_string(), "FG-54F07");

	let store = Datastore::in_memory_with(InMemoryOptions {
		max_snapshot_age: Duration::ZERO,
		max_write_set_bytes: DEFAULT_MAX_WRITE_SET_BYTES,
	});
	assert_eq!(store.begin().commit().unwrap_err().status().code().to_string(), "FG-25F01");
}

#[test]
fn write_set_limit_counts_large_property_payload_before_publication() {
	let store = Datastore::in_memory_with(InMemoryOptions {
		max_snapshot_age: Duration::from_secs(300),
		max_write_set_bytes: 1_024,
	});
	let payload = store.intern_property("payload").unwrap();
	let mut create = store.begin();
	let graph = create
		.create_graph(graph_id("large-write"), Graph::<Mutable>::new().freeze().into())
		.unwrap();
	create.commit().unwrap();

	let mut transaction = store.begin();
	let GraphWriteView::Graph(write) = transaction.edit_graph(&graph).unwrap() else {
		panic!("test graph must keep the general graph family");
	};
	let mut properties = PropertyMap::new();
	properties.insert(payload, Bytes::from(vec![0_u8; 2_048]));
	write.add_node(None, properties).unwrap();
	assert_eq!(transaction.commit().unwrap_err().status().code().to_string(), "FG-54F07");

	let read = store.begin();
	assert_eq!(graph.read(&read).unwrap().node_count().unwrap(), 0);
}

#[cfg(not(all(windows, feature = "native-async-io")))]
#[test]
fn unavailable_native_async_mode_has_no_sync_fallback() {
	let error = validate_local_options(LocalOptions {
		io_mode: IoMode::NativeAsync,
		..LocalOptions::default()
	})
	.unwrap_err();
	assert_eq!(error.status().code().to_string(), "FG-51I04");
}

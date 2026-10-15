#![cfg(test)]

//! Direct SDK checks independent of GQL execution.
#![allow(
	clippy::unwrap_used,
	reason = "test setup and assertions fail on every unexpected SDK error"
)]
/// Assertions shared with the root unit-test harness.
#[path = "common/assertions.rs"]
pub mod test_assertions;

use ferros_grynn::{
	datastore::{ClassSchema, Datastore},
	graph::{
		AtomKind,
		EdgeEndpoint,
		EdgeTopology,
		Graph,
		GraphKind,
		GraphRead,
		Mutable,
		QualifiedAtomId,
		TraversalDirection,
		TraversalIndex,
		TraversalSemantics,
		tensor::{
			CsrMatrix,
			EndpointTensor,
			GraphAlgorithmLimits,
			IncidenceSide,
			ProjectionOptions,
			TensorAtom,
			TensorOptions,
			TensorScope,
		},
	},
	values::{Number, PropertyMap, RecordId},
};

/// Checks each family through the same public read/tensor boundary.
fn check_family<G: GraphRead>(graph: &G) {
	let tensor = EndpointTensor::from_graph(graph, TensorOptions::default()).unwrap();
	assert_eq!(tensor.incidence_tensor().shape().0, graph.node_count().unwrap());
	assert_eq!(tensor.connectors().len(), graph.edge_count().unwrap());
	for mode in [TraversalSemantics::Node, TraversalSemantics::Atom] {
		let oracle = TraversalIndex::new(graph, mode).unwrap();
		for direction in
			[TraversalDirection::Forward, TraversalDirection::Backward, TraversalDirection::Both]
		{
			let mut options = ProjectionOptions::default();
			options.mode = mode;
			options.direction = direction;
			let projection = tensor.project(options).unwrap();
			for (row, atom) in projection.atoms().iter().enumerate() {
				let source = EdgeEndpoint::Local(graph.atom_id(atom.atom()).unwrap().unwrap());
				for (column, target) in projection.atoms().iter().enumerate() {
					let target =
						EdgeEndpoint::Local(graph.atom_id(target.atom()).unwrap().unwrap());
					assert_eq!(
						projection.binary_adjacency().csr().get(row, column).unwrap().is_some(),
						oracle.steps(&source, direction).any(|step| step.target() == &target)
					);
				}
			}
		}
	}
}

/// Verifies tensor api accepts every runtime graph family.
#[test]
fn tensor_api_accepts_every_runtime_graph_family() {
	use ferros_grynn::graph::{
		Dag,
		Forest,
		Hypergraph,
		Metagraph,
		RootedTree,
		Subgraph,
		Tree,
		TreeOrientation,
	};
	let mut dag = Dag::<Mutable>::new();
	let a = dag.add_node(None, PropertyMap::new()).unwrap();
	let b = dag.add_node(None, PropertyMap::new()).unwrap();
	dag.add_edge(a, b, None, PropertyMap::new()).unwrap();
	check_family(&dag);
	let mut forest = Forest::<Mutable>::new(TreeOrientation::InArborescence);
	let a = forest.add_root(None, PropertyMap::new()).unwrap();
	let b = forest.add_root(None, PropertyMap::new()).unwrap();
	forest.connect(a, b, None, PropertyMap::new()).unwrap();
	check_family(&forest);
	let mut tree =
		Tree::<Mutable>::new(TreeOrientation::Undirected, None, PropertyMap::new()).unwrap();
	let root = tree.roots().next().unwrap();
	tree.add_child(root, None, PropertyMap::new(), None, PropertyMap::new()).unwrap();
	check_family(&tree);
	let mut rooted =
		RootedTree::<Mutable>::new(TreeOrientation::OutArborescence, None, PropertyMap::new())
			.unwrap();
	let root = rooted.roots().next().unwrap();
	rooted.add_child(root, None, PropertyMap::new(), None, PropertyMap::new()).unwrap();
	check_family(&rooted);
	let mut hyper = Hypergraph::<Mutable>::new();
	let mut nodes = Vec::new();
	for _ in 0..4 {
		nodes.push(hyper.add_node(None, PropertyMap::new()).unwrap());
	}
	hyper
		.add_edge(
			EdgeTopology::directed(nodes[..2].iter().copied(), nodes[2..].iter().copied()),
			None,
			PropertyMap::new(),
		)
		.unwrap();
	check_family(&hyper);
	let mut meta = Metagraph::<Mutable>::new();
	let a = meta.add_node(None, PropertyMap::new()).unwrap();
	let b = meta.add_node(None, PropertyMap::new()).unwrap();
	meta.add_edge(EdgeTopology::bidirectional([a], [b]), None, PropertyMap::new()).unwrap();
	check_family(&meta);
	let mut general = Graph::<Mutable>::new();
	let a = general.add_node(None, PropertyMap::new()).unwrap();
	let b = general.add_node(None, PropertyMap::new()).unwrap();
	general.add_edge(EdgeTopology::undirected([a, b]), None, PropertyMap::new()).unwrap();
	check_family(&general);
	check_family(&Subgraph::from_graph(general));
}

/// Verifies tensor api generated directed graphs match independent pair oracle.
#[test]
fn tensor_api_generated_directed_graphs_match_independent_pair_oracle() {
	for bits in 0_u64..64 {
		let mut graph = Graph::<Mutable>::new();
		let mut ids = Vec::new();
		for node in 0..3 {
			ids.push(
				graph
					.add_node_with_id(
						format!("n:{node}").parse().unwrap(),
						None,
						PropertyMap::new(),
					)
					.unwrap(),
			);
		}
		let mut expected = [[false; 3]; 3];
		let mut bit = 0;
		for source in 0..3 {
			for target in 0..3 {
				if source == target {
					continue;
				}
				if bits & (1 << bit) != 0 {
					graph
						.add_edge(
							EdgeTopology::directed([ids[source]], [ids[target]]),
							None,
							PropertyMap::new(),
						)
						.unwrap();
					expected[source][target] = true;
				}
				bit += 1;
			}
		}
		let tensor = EndpointTensor::from_graph(&graph, TensorOptions::default()).unwrap();
		for direction in
			[TraversalDirection::Forward, TraversalDirection::Backward, TraversalDirection::Both]
		{
			let mut options = ProjectionOptions::default();
			options.direction = direction;
			let projection = tensor.project(options).unwrap();
			for source in 0..3 {
				for target in 0..3 {
					let expected = match direction {
						TraversalDirection::Forward => expected[source][target],
						TraversalDirection::Backward => expected[target][source],
						TraversalDirection::Both => {
							expected[source][target] || expected[target][source]
						}
						_ => unreachable!(),
					};
					assert_eq!(
						projection.binary_adjacency().csr().get(source, target).unwrap().is_some(),
						expected
					);
				}
			}
		}
	}
}

/// Verifies tensor api projects every incidence direction and retains zero
/// links.
#[test]
fn tensor_api_projects_every_incidence_direction_and_retains_zero_links() {
	let mut graph = Graph::<Mutable>::new();
	let a = graph.add_node_with_id("n:a".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let b = graph.add_node_with_id("n:b".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let c = graph.add_node_with_id("n:c".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let edge = graph
		.add_edge_with_id(
			"e:base".parse().unwrap(),
			EdgeTopology::directed([a], [b]),
			None,
			PropertyMap::new(),
		)
		.unwrap();
	graph.set_edge_weight(edge, Some(Number::Int(3))).unwrap();
	let cancel = graph
		.add_edge_with_id(
			"e:cancel".parse().unwrap(),
			EdgeTopology::directed([a], [b]),
			None,
			PropertyMap::new(),
		)
		.unwrap();
	graph.set_edge_weight(cancel, Some(Number::Int(-3))).unwrap();
	graph
		.add_edge(EdgeTopology::bidirectional([a, edge], [c, b]), None, PropertyMap::new())
		.unwrap();
	graph.add_edge(EdgeTopology::undirected([a, b, c]), None, PropertyMap::new()).unwrap();
	graph.add_edge(EdgeTopology::directed([c], [c]), None, PropertyMap::new()).unwrap();
	let tensor = EndpointTensor::from_graph(&graph, TensorOptions::default()).unwrap();
	assert_eq!(tensor.shape(), (8, 5, 2));
	assert_eq!(tensor.incidence_tensor().shape(), (3, 5, 2));
	assert!(tensor.get(99, 0, IncidenceSide::Both).is_err());
	for (column, connector) in tensor.connectors().iter().enumerate() {
		for endpoint in connector.source().iter().chain(connector.target()) {
			let row = endpoint.row();
			let undirected =
				connector.direction() == ferros_grynn::graph::EdgeDirection::Undirected;
			let source =
				undirected || connector.source().iter().any(|endpoint| endpoint.row() == row);
			let target =
				undirected || connector.target().iter().any(|endpoint| endpoint.row() == row);
			assert_eq!(tensor.get(row, column, IncidenceSide::Source).unwrap(), source);
			assert_eq!(tensor.get(row, column, IncidenceSide::Target).unwrap(), target);
			assert!(tensor.get(row, column, IncidenceSide::Both).unwrap());
		}
	}
	for side in [IncidenceSide::Source, IncidenceSide::Target, IncidenceSide::Both] {
		let view = tensor.matrix(side);
		let csr = view.to_csr(GraphAlgorithmLimits::default()).unwrap();
		let dense = view.to_dense(GraphAlgorithmLimits::default()).unwrap();
		for row in 0..view.shape().0 {
			for col in 0..view.shape().1 {
				assert_eq!(*dense.get(row, col).unwrap(), view.get(row, col).unwrap());
				assert_eq!(csr.get(row, col).unwrap().is_some(), view.get(row, col).unwrap());
			}
		}
	}
	for mode in [TraversalSemantics::Node, TraversalSemantics::Atom] {
		let oracle = TraversalIndex::new(&graph, mode).unwrap();
		for direction in
			[TraversalDirection::Forward, TraversalDirection::Backward, TraversalDirection::Both]
		{
			let mut options = ProjectionOptions::default();
			options.mode = mode;
			options.direction = direction;
			let projection = tensor.project(options).unwrap();
			for (row, identity) in projection.atoms().iter().enumerate() {
				let id = graph.atom_id(identity.atom()).unwrap().unwrap();
				for (column, other) in projection.atoms().iter().enumerate() {
					let target = graph.atom_id(other.atom()).unwrap().unwrap();
					let expected = oracle
						.steps(&EdgeEndpoint::Local(id), direction)
						.any(|step| step.target() == &EdgeEndpoint::Local(target));
					assert_eq!(
						projection.binary_adjacency().csr().get(row, column).unwrap().is_some(),
						expected,
						"{mode:?} {direction:?} {row} {column}"
					);
				}
			}
			for source in [AtomKind::Node, AtomKind::Edge] {
				for target in [AtomKind::Node, AtomKind::Edge] {
					let block = projection.weighted_adjacency().block(source, target);
					assert_eq!(
						block.to_csr(GraphAlgorithmLimits::default()).unwrap().shape(),
						block.shape()
					);
				}
			}
		}
	}
	let mut options = ProjectionOptions::default();
	options.labels.insert("absent".into());
	assert_eq!(tensor.project(options).unwrap().binary_adjacency().csr().stored_cells(), 0);
	let other = Graph::<Mutable>::new();
	assert!(tensor.validate_binding(&other).is_err());
	assert!(tensor.validate_binding(&graph).is_ok());
	graph.add_node(None, PropertyMap::new()).unwrap();
	assert!(tensor.validate_binding(&graph).is_err());
}

/// Verifies tensor api preserves qualified domains and shared child
/// containment.
#[test]
fn tensor_api_preserves_qualified_domains_and_shared_child_containment() {
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	for (class, kind) in [("n", AtomKind::Node), ("e", AtomKind::Edge)] {
		tx.define_class(ClassSchema::new(class, kind, []).unwrap()).unwrap();
	}
	let parent = tx.define_graph("parent", GraphKind::Metagraph).unwrap();
	let child = tx.define_graph("child", GraphKind::Graph).unwrap();
	let records = ["n:a", "n:b", "n:c1", "n:c2", "e:base", "e:cross"]
		.map(|text| text.parse::<RecordId>().unwrap());
	for record in &records {
		tx.create_atom(record.clone(), PropertyMap::new()).unwrap();
	}
	let a = tx.attach_node(child.id(), &records[0], None, None).unwrap();
	let b = tx.attach_node(child.id(), &records[1], None, None).unwrap();
	let endpoint = tx
		.attach_edge(
			child.id(),
			&records[4],
			None,
			EdgeTopology::directed([a], [b]),
			Some(Number::Int(0)),
			None,
		)
		.unwrap();
	let first = tx.attach_node(parent.id(), &records[2], None, Some(child.id())).unwrap();
	let second = tx.attach_node(parent.id(), &records[3], None, Some(child.id())).unwrap();
	tx.attach_node(parent.id(), &records[0], None, None).unwrap();
	let q1 = EdgeEndpoint::Nested(QualifiedAtomId::new(vec![first], endpoint).unwrap());
	let q2 = EdgeEndpoint::Nested(QualifiedAtomId::new(vec![second], endpoint).unwrap());
	tx.attach_edge(
		parent.id(),
		&records[5],
		None,
		EdgeTopology::directed([q1], [q2, EdgeEndpoint::Local(second)]),
		None,
		None,
	)
	.unwrap();
	tx.commit().unwrap();
	let mut read = store.begin();
	let local = read.endpoint_tensor(&parent, TensorOptions::default()).unwrap();
	assert_eq!(local.connectors().len(), 1);
	assert_eq!(local.containment().len(), 2);
	assert_eq!(local.connectors()[0].source()[0].owners().len(), 1);
	let mut options = ProjectionOptions::default();
	options.mode = TraversalSemantics::Atom;
	assert_eq!(local.project(options.clone()).unwrap().binary_adjacency().csr().stored_cells(), 0);
	let mut rooted_options = TensorOptions::default();
	rooted_options.scope = TensorScope::Rooted;
	let rooted = read.endpoint_tensor(&parent, rooted_options).unwrap();
	assert_eq!(rooted.connectors().len(), 2);
	assert_eq!(rooted.containment().len(), 2);
	assert_eq!(rooted.atoms().iter().filter(|atom| atom.atom().id() == &records[0]).count(), 2);
	assert!(rooted.atoms().iter().all(|atom| matches!(atom, TensorAtom::Occurrence(_))));
	let projection = rooted.project(options).unwrap();
	assert_eq!(projection.binary_adjacency().csr().stored_cells(), 3);
	assert!(
		projection
			.weighted_adjacency()
			.csr()
			.entries()
			.any(|(_, _, value)| *value == Number::Int(0))
	);
	drop(read);
	assert_eq!(rooted.connectors().len(), 2);
}

/// Verifies tensor api rejects malformed csr excessive exports and
/// cancellation.
#[test]
fn tensor_api_rejects_malformed_csr_excessive_exports_and_cancellation() {
	let limits = GraphAlgorithmLimits::default();
	for offsets in [vec![0, 2, 1], vec![1, 1, 1], vec![0, usize::MAX, 1]] {
		assert!(CsrMatrix::try_new((2, 2), offsets, vec![0], vec![true], limits).is_err());
	}
	assert!(CsrMatrix::try_new((1, 3), vec![0, 2], vec![1, 1], vec![true, true], limits).is_err());
	let sparse = CsrMatrix::try_new(
		(usize::MAX - 1, 0),
		Vec::new(),
		Vec::<usize>::new(),
		Vec::<bool>::new(),
		limits,
	);
	assert!(sparse.is_err());
	let matrix = CsrMatrix::try_new(
		(1, usize::MAX),
		vec![0, 0],
		Vec::<usize>::new(),
		Vec::<Number>::new(),
		limits,
	)
	.unwrap();
	assert!(matrix.to_dense(limits).is_err());
	let graph = Graph::<Mutable>::new();
	assert!(
		EndpointTensor::from_graph_with_check(&graph, TensorOptions::default(), &mut || Err(
			ferros_grynn::gql_status::FerrosGrynnError::query_cancelled()
		))
		.is_err()
	);
	let empty = EndpointTensor::from_graph(&graph, TensorOptions::default()).unwrap();
	assert_eq!(empty.shape(), (0, 0, 2));
	assert_eq!(empty.project(ProjectionOptions::default()).unwrap().atoms().len(), 0);
	let mut small = TensorOptions::default();
	small.limits.memory_bytes = 1;
	assert!(EndpointTensor::from_graph(&graph, small).is_err());
}

/// Verifies tensor api fallback does not allocate metadata for other index
/// families.
#[test]
fn tensor_api_fallback_does_not_allocate_metadata_for_other_index_families() {
	use ferros_grynn::datastore::{GraphIndex, GraphIndexDefinition, ReachabilityAlgorithm};
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let graph = tx.define_graph("definitions", GraphKind::Graph).unwrap();
	for ordinal in 0..128 {
		tx.define_graph_index(
			"definitions",
			GraphIndex::new(
				format!("reach_{ordinal}"),
				GraphIndexDefinition::Reachability {
					algorithm: ReachabilityAlgorithm::OnDemand,
				},
				None,
			)
			.unwrap(),
		)
		.unwrap();
	}
	tx.commit().unwrap();
	let mut options = TensorOptions::default();
	options.limits.memory_bytes = 4096;
	let tensor = store.begin().endpoint_tensor(&graph, options).unwrap();
	assert_eq!(tensor.shape(), (0, 0, 2));
}

/// Verifies tensor api deep rooted and qualified routes use bounded iterative
/// borrows.
#[test]
fn tensor_api_deep_rooted_and_qualified_routes_use_bounded_iterative_borrows() {
	let mut graph = Graph::<Mutable>::new();
	let leaf = graph.add_node_with_id("n:leaf".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let mut scope = Vec::new();
	for _ in 0..96 {
		let mut parent = Graph::<Mutable>::new();
		let owner = parent.add_metavertex(graph.freeze(), None, PropertyMap::new()).unwrap();
		scope.insert(0, owner);
		graph = parent;
	}
	let source = graph.add_node(None, PropertyMap::new()).unwrap();
	graph
		.add_edge(
			EdgeTopology::directed([EdgeEndpoint::Local(source)], [EdgeEndpoint::Nested(
				QualifiedAtomId::new(scope, leaf).unwrap(),
			)]),
			None,
			PropertyMap::new(),
		)
		.unwrap();
	let local = EndpointTensor::from_graph(&graph, TensorOptions::default()).unwrap();
	assert_eq!(local.connectors()[0].target()[0].owners().len(), 96);
	assert_eq!(local.atoms().len(), 99);
	assert_eq!(local.domain_atoms().count(), 3);
	let mut options = TensorOptions::default();
	options.scope = TensorScope::Rooted;
	let rooted = EndpointTensor::from_graph(&graph, options).unwrap();
	assert_eq!(rooted.atoms().len(), 99);
	assert_eq!(rooted.domain_atoms().count(), 99);
	assert_eq!(rooted.bindings().len(), 97);
	assert_eq!(rooted.containment().len(), 96);
	assert_eq!(
		rooted
			.project(ProjectionOptions::default())
			.unwrap()
			.binary_adjacency()
			.csr()
			.stored_cells(),
		1
	);
	options.limits.memory_bytes = 4096;
	assert!(EndpointTensor::from_graph(&graph, options).is_err());
	options.limits = GraphAlgorithmLimits::default();
	options.limits.max_work = 64;
	assert!(EndpointTensor::from_graph(&graph, options).is_err());
}

/// Verifies tensor api local retains containment without scanning child.
#[test]
fn tensor_api_local_retains_containment_without_scanning_child() {
	let mut child = Graph::<Mutable>::new();
	child.add_node(None, PropertyMap::new()).unwrap();
	let mut parent = Graph::<Mutable>::new();
	parent.add_metavertex(child.freeze(), None, PropertyMap::new()).unwrap();
	let local = EndpointTensor::from_graph(&parent, TensorOptions::default()).unwrap();
	assert_eq!(local.containment().len(), 1);
	assert_eq!(local.atoms().len(), 1);
	let mut options = TensorOptions::default();
	options.scope = TensorScope::Rooted;
	let rooted = EndpointTensor::from_graph(&parent, options).unwrap();
	assert_eq!(rooted.atoms().len(), 2);
	assert_eq!(
		rooted
			.project(ProjectionOptions::default())
			.unwrap()
			.binary_adjacency()
			.csr()
			.stored_cells(),
		0
	);
}

/// Verifies tensor api kind filtered export counts rejected cells.
#[test]
fn tensor_api_kind_filtered_export_counts_rejected_cells() {
	let mut empty_graph = Graph::<Mutable>::new();
	for _ in 0..8 {
		empty_graph.add_node(None, PropertyMap::new()).unwrap();
	}
	let empty_tensor = EndpointTensor::from_graph(&empty_graph, TensorOptions::default()).unwrap();
	let mut low_work = GraphAlgorithmLimits::default();
	low_work.max_work = 1;
	assert!(empty_tensor.matrix(IncidenceSide::Both).to_dense(low_work).is_err());
	assert!(empty_tensor.matrix(IncidenceSide::Both).to_csr(low_work).is_err());
	let empty_csr = CsrMatrix::try_new(
		(8, 0),
		vec![0; 9],
		Vec::new(),
		Vec::<bool>::new(),
		GraphAlgorithmLimits::default(),
	)
	.unwrap();
	assert!(empty_csr.to_dense(low_work).is_err());
	let empty_projection = empty_tensor.project(ProjectionOptions::default()).unwrap();
	assert!(
		empty_projection
			.binary_adjacency()
			.block(AtomKind::Node, AtomKind::Edge)
			.to_dense(low_work)
			.is_err()
	);
	let mut low_projection = ProjectionOptions::default();
	low_projection.limits = low_work;
	assert!(empty_tensor.project(low_projection).is_err());
	let mut offsets = Vec::with_capacity(16 * 1024);
	offsets.extend([0, 0]);
	let mut small = GraphAlgorithmLimits::default();
	small.memory_bytes = 2048;
	assert!(CsrMatrix::try_new((1, 0), offsets, Vec::new(), Vec::<bool>::new(), small).is_err());
	let mut graph = Graph::<Mutable>::new();
	let a = graph.add_node(None, PropertyMap::new()).unwrap();
	let mut targets = Vec::new();
	for _ in 0..16 {
		targets.push(
			graph.add_edge(EdgeTopology::directed([a], [a]), None, PropertyMap::new()).unwrap(),
		);
	}
	graph.add_edge(EdgeTopology::directed([a], targets), None, PropertyMap::new()).unwrap();
	let tensor = EndpointTensor::from_graph(&graph, TensorOptions::default()).unwrap();
	let mut options = ProjectionOptions::default();
	options.mode = TraversalSemantics::Atom;
	let projection = tensor.project(options).unwrap();
	let vv = projection.binary_adjacency().block(AtomKind::Node, AtomKind::Node);
	let mut limits = GraphAlgorithmLimits::default();
	limits.max_work = 2;
	assert!(vv.to_csr(limits).is_err());
	assert!(vv.to_dense(limits).is_err());
	assert!(
		CsrMatrix::try_new((1, 16), vec![0, 16], (0..16).collect(), vec![true; 16], limits)
			.is_err()
	);
}

/// Verifies tensor api tensor admission ignores unrelated large overlay values.
#[test]
fn tensor_api_tensor_admission_ignores_unrelated_large_overlay_values() {
	use ferros_grynn::values::{PropertyKeyId, Value};
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("n", AtomKind::Node, []).unwrap()).unwrap();
	let graph = tx.define_graph("overlay", GraphKind::Graph).unwrap();
	let record = "n:large".parse::<RecordId>().unwrap();
	let mut properties = PropertyMap::new();
	properties.insert(PropertyKeyId::new(7), "x".repeat(4 * 1024 * 1024));
	tx.create_atom(record.clone(), properties).unwrap();
	tx.attach_node(graph.id(), &record, None, None).unwrap();
	tx.set_overlay(graph.id(), &record, PropertyKeyId::new(8), Value::from("overlay")).unwrap();
	tx.commit().unwrap();
	let mut read = store.begin();
	let mut options = TensorOptions::default();
	options.limits.memory_bytes = 64 * 1024;
	let tensor = read.endpoint_tensor(&graph, options).unwrap();
	assert_eq!(tensor.atoms().len(), 1);
	assert!(tensor.admitted_bytes() < 64 * 1024);
}

/// Verifies tensor cache sdk build snapshot private writes dependencies and
/// maintenance.
#[test]
fn tensor_cache_sdk_build_snapshot_private_writes_dependencies_and_maintenance() {
	use ferros_grynn::{
		datastore::{
			ClassIndexState,
			GraphIndex,
			MaintenanceJobState,
			MaintenanceOptions,
			TensorCacheFormat,
		},
		values::{PropertyKeyId, Value},
	};
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("n", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("e", AtomKind::Edge, []).unwrap()).unwrap();
	let child = seed.define_graph("child", GraphKind::Graph).unwrap();
	let parent = seed.define_graph("parent", GraphKind::Graph).unwrap();
	let records = ["n:a", "n:b", "n:owner", "e:ab"].map(|id| id.parse::<RecordId>().unwrap());
	for record in &records {
		seed.create_atom(record.clone(), PropertyMap::new()).unwrap();
	}
	let a = seed.attach_node(child.id(), &records[0], None, None).unwrap();
	let b = seed.attach_node(child.id(), &records[1], None, None).unwrap();
	seed.attach_edge(
		child.id(),
		&records[3],
		None,
		EdgeTopology::directed([a], [b]),
		Some(Number::Int(0)),
		None,
	)
	.unwrap();
	seed.attach_node(parent.id(), &records[2], None, Some(child.id())).unwrap();
	let id = seed
		.define_graph_index(
			"parent",
			GraphIndex::new_tensor(
				"tensor",
				TensorScope::Rooted,
				TensorCacheFormat::Dense,
				1 << 20,
			)
			.unwrap(),
		)
		.unwrap();
	seed.commit().unwrap();
	let mut options = TensorOptions::default();
	options.scope = TensorScope::Rooted;
	assert!(store.begin().endpoint_tensor_using_index(&parent, options, id).is_err());
	store.rebuild_graph_index(parent.id(), id, MaintenanceOptions::default()).unwrap();
	let mut old = store.begin();
	let expected = old.endpoint_tensor_using_index(&parent, options, id).unwrap();
	assert_eq!(expected.connectors().len(), 1);
	assert_eq!(expected.connectors()[0].weight(), Number::Int(0));
	assert_eq!(
		old.graph_index("parent", "tensor").unwrap().unwrap().state(),
		ClassIndexState::Ready
	);
	assert!(
		store.begin().endpoint_tensor_using_index(&parent, TensorOptions::default(), id).is_err()
	);
	let mut private = store.begin();
	private.detach_occurrence(child.id(), &records[3], false).unwrap();
	assert!(private.endpoint_tensor_using_index(&parent, options, id).is_err());
	assert!(private.endpoint_tensor(&parent, options).unwrap().connectors().is_empty());
	drop(private);
	assert!(store.begin().endpoint_tensor_using_index(&parent, options, id).is_ok());
	let mut properties = store.begin();
	properties
		.set_base_property(&records[0], PropertyKeyId::new(17), Value::from("property"))
		.unwrap();
	properties.commit().unwrap();
	let mut read = store.begin();
	let current = read.endpoint_tensor_using_index(&parent, options, id).unwrap();
	assert_ne!(current.root_binding().snapshot(), expected.root_binding().snapshot());
	assert_eq!(current.containment()[0].child().snapshot(), current.root_binding().snapshot());
	let mut small = options;
	small.limits.memory_bytes = 1024;
	assert!(read.endpoint_tensor_using_index(&parent, small, id).is_err());
	let mut writer = store.begin();
	writer.detach_occurrence(child.id(), &records[3], false).unwrap();
	writer.commit().unwrap();
	read.create_atom("n:observer".parse().unwrap(), PropertyMap::new()).unwrap();
	assert!(read.commit().is_err());
	assert!(store.begin().endpoint_tensor_using_index(&parent, options, id).is_err());
	assert!(store.begin().endpoint_tensor(&parent, options).unwrap().connectors().is_empty());
	assert_eq!(
		old.endpoint_tensor_using_index(&parent, options, id).unwrap().connectors(),
		expected.connectors()
	);
	drop(old);
	let job = store.rebuild_graph_index_concurrently(parent.id(), id, None).unwrap();
	let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
	loop {
		let info = store.job(job).unwrap().unwrap();
		if info.state() == MaintenanceJobState::Completed {
			break;
		}
		assert!(
			!matches!(
				info.state(),
				MaintenanceJobState::Failed
					| MaintenanceJobState::Cancelled
					| MaintenanceJobState::Interrupted
			),
			"{info:?}"
		);
		assert!(std::time::Instant::now() < deadline, "{info:?}");
		std::thread::sleep(std::time::Duration::from_millis(10));
	}
	assert!(
		store
			.begin()
			.endpoint_tensor_using_index(&parent, options, id)
			.unwrap()
			.connectors()
			.is_empty()
	);
	let mut remove = store.begin();
	remove.remove_graph_index("parent", "tensor").unwrap();
	remove.commit().unwrap();
	assert!(store.begin().graph_index("parent", "tensor").unwrap().is_none());
	assert!(store.begin().endpoint_tensor_using_index(&parent, options, id).is_err());
}

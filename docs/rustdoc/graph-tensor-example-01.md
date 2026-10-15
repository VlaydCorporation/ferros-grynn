```
use ferros_grynn::{
	graph::{
		AtomKind,
		EdgeTopology,
		Graph,
		Mutable,
		tensor::{
			EndpointTensor,
			GraphAlgorithmLimits,
			IncidenceSide,
			ProjectionOptions,
			TensorOptions,
		},
	},
	values::PropertyMap,
};
# fn main() -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
let mut graph = Graph::<Mutable>::new();
let a = graph.add_node(None, PropertyMap::new())?;
let b = graph.add_node(None, PropertyMap::new())?;
graph.add_edge(EdgeTopology::directed([a], [b]), None, PropertyMap::new())?;
let tensor = EndpointTensor::from_graph(&graph, TensorOptions::default())?;
assert_eq!(tensor.incidence_tensor().shape(), (2, 1, 2));
let atom = &tensor.atoms()[0];
assert_eq!(tensor.atom_ordinal(atom), Some(0));
assert_eq!(tensor.connector_ordinal(tensor.connectors()[0].atom()), Some(0));
assert_eq!(tensor.nonzeros().count(), 2);
for side in [IncidenceSide::Source, IncidenceSide::Target, IncidenceSide::Both] {
	let full = tensor.matrix(side);
	assert_eq!(full.shape(), (3, 1));
	assert_eq!(full.get(0, 0)?, tensor.get(0, 0, side)?);
	let nodes = tensor.incidence_tensor().matrix(side);
	assert_eq!(nodes.get(0, 0)?, tensor.incidence_tensor().get(0, 0, side)?);
}
let plane = tensor.incidence_tensor().matrix(IncidenceSide::Both);
assert_eq!(plane.to_csr(GraphAlgorithmLimits::default())?.stored_cells(), 2);
assert_eq!(plane.to_dense(GraphAlgorithmLimits::default())?.shape(), (2, 1));
let adjacency = tensor.project(ProjectionOptions::default())?;
let binary = adjacency.binary_adjacency();
assert_eq!(binary.csr().stored_cells(), 1);
for (source, target, shape) in [
	(AtomKind::Node, AtomKind::Node, (2, 2)), // VV
	(AtomKind::Node, AtomKind::Edge, (2, 0)), // VE
	(AtomKind::Edge, AtomKind::Node, (0, 2)), // EV
	(AtomKind::Edge, AtomKind::Edge, (0, 0)), // EE
] {
	let block = binary.block(source, target);
	assert_eq!(block.shape(), shape);
	assert_eq!(block.to_csr(GraphAlgorithmLimits::default())?.shape(), shape);
	let weighted = adjacency.weighted_adjacency().block(source, target);
	assert_eq!(weighted.to_dense(GraphAlgorithmLimits::default())?.shape(), shape);
}
# Ok(()) }
```

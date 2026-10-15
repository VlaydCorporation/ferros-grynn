```
use ferros_grynn::{
	graph::{EdgeTopology, Graph, Mutable, analytics::*, tensor::*},
	values::PropertyMap,
};
# fn main() -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
let mut graph = Graph::<Mutable>::new();
let a = graph.add_node_with_id("node:a".parse()?, None, PropertyMap::new())?;
let b = graph.add_node_with_id("node:b".parse()?, None, PropertyMap::new())?;
graph.add_edge(EdgeTopology::directed([a], [b]), None, PropertyMap::new())?;
let tensor = EndpointTensor::from_graph(&graph, TensorOptions::default())?;
let axes: Vec<_> = tensor
	.domain_atoms()
	.filter(|a| a.atom().kind() == ferros_grynn::graph::AtomKind::Node)
	.collect();
let complete = tensor
	.all_shortest_paths(
		axes[0],
		axes[1],
		ProjectionOptions::default(),
		AllShortestOptions::default(),
	)?
	.collect_all()?;
assert_eq!(complete.len(), 1);
let mut options = AllShortestOptions::default();
options.cost = PathCost::Custom;
let result = tensor.all_shortest_paths_with_cost(
	axes[0],
	axes[1],
	ProjectionOptions::default(),
	options,
	&mut |context| Ok(context.prefix),
	&mut || Ok(()),
)?;
assert_eq!(result.enumerate().count(), 1);
# Ok(()) }
```

```
# extern crate fg_runtime as ferros_grynn;
use ferros_grynn::graph::{Graph, Mutable, analytics, tensor::*};
# fn main() -> fg_gql_status::FerrosGrynnResult<()> {
let graph = Graph::<Mutable>::new();
let tensor = EndpointTensor::from_graph(&graph, TensorOptions::default())?;
let projection = tensor.project(ProjectionOptions::default())?;
let mut check = || Ok(());
analytics::degree(&projection, GraphAlgorithmLimits::default(), &mut check)?;
analytics::page_rank(&projection, analytics::PageRankOptions::default(), &mut check)?;
analytics::harmonic_closeness(
	&projection,
	analytics::CentralityOptions::default(),
	&mut check,
)?;
analytics::betweenness(&projection, analytics::CentralityOptions::default(), &mut check)?;
analytics::eigenvector(&projection, analytics::SpectralOptions::default(), &mut check)?;
analytics::katz(&projection, analytics::KatzOptions::default(), &mut check)?;
analytics::triangles(&projection, analytics::TriangleOptions::default(), &mut check)?;
analytics::clustering(&projection, analytics::ClusteringOptions::default(), &mut check)?;
analytics::label_propagation(
	&projection,
	analytics::LabelPropagationOptions::default(),
	&mut check,
)?;
analytics::louvain(&projection, analytics::LouvainOptions::default(), &mut check)?;
# Ok(())
# }
```

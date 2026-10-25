```
# use fg_engine as ferros_grynn;
use ferros_grynn::{
	datastore::{Datastore, GraphIndex, MaintenanceOptions, TensorCacheFormat},
	graph::{
		GraphKind,
		tensor::{TensorOptions, TensorScope},
	},
};
# fn main() -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
let store = Datastore::in_memory();
let mut tx = store.begin();
let graph = tx.define_graph("example", GraphKind::Graph)?;
let id = tx.define_graph_index(
	"example",
	GraphIndex::new_tensor("incidence", TensorScope::Local, TensorCacheFormat::Auto, 1 << 20)?,
)?;
tx.commit()?;
store.rebuild_graph_index(graph.id(), id, MaintenanceOptions::default())?;
let mut read = store.begin();
let tensor = read.endpoint_tensor_using_index(&graph, TensorOptions::default(), id)?;
assert_eq!(tensor.shape(), (0, 0, 2));
assert!(read.graph_index("example", "incidence")?.is_some());
read.remove_graph_index("example", "incidence")?;
read.commit()?;
# Ok(())
# }
```

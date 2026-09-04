//! Bounded two-pass materialization of one local graph generation.

use super::*;
use crate::graph::remap_topology;

/// Builds owned graph data while reporting each exact source atom read.
pub(super) fn materialize_query_graph(
	store: &LocalStore,
	view: &LocalGraphView<'_>,
	mut record_atom: impl FnMut(AtomId),
) -> FerrosGrynnResult<(Graph<Mutable>, BTreeMap<AtomId, AtomRef>)> {
	view.transaction.ensure_live()?;
	let node_count = usize::try_from(view.graph.superblock.node_count)
		.map_err(|_error| FerrosGrynnError::invalid_storage_file("node count exceeds usize"))?;
	let edge_count = usize::try_from(view.graph.superblock.edge_count)
		.map_err(|_error| FerrosGrynnError::invalid_storage_file("edge count exceeds usize"))?;
	let properties = GraphRead::properties(view)?.into_owned();
	let context = view.graph.decode_context(store, &view.transaction)?;
	let mut result =
		Graph::<Mutable>::begin_query_materialization(properties, node_count, edge_count);
	let mut ids = BTreeMap::new();
	let mut source_atoms = BTreeMap::new();

	visit_local_primary(store, &view.transaction, &view.graph, |key, disk| {
		let record_id = decode_record_key(key, &context)?;
		let source_id = local_atom_id(store, &view.transaction, &view.graph, disk)?;
		let source_ref = AtomRef::new(disk.kind(), record_id.clone());
		let local_id = match disk.kind() {
			AtomKind::Node => {
				let node = read_local_node(
					store,
					&view.transaction,
					&view.graph,
					source_id,
					record_id,
					Some(view.transaction.clone()),
				)?
				.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"primary index references a missing node",
					)
				})?;
				result.materialize_query_node(
					node.record_id()?.into_owned(),
					node.label_text()?.map(Cow::into_owned),
					node.properties()?.into_owned(),
					node.inner_graph()?.map(Cow::into_owned),
				)?
			}
			AtomKind::Edge => {
				let edge = read_local_edge(
					store,
					&view.transaction,
					&view.graph,
					source_id,
					record_id,
					Some(view.transaction.clone()),
				)?
				.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"primary index references a missing edge",
					)
				})?;
				result.reserve_query_edge(
					edge.record_id()?.into_owned(),
					edge.label_text()?.map(Cow::into_owned),
					edge.properties()?.into_owned(),
					edge.weight(),
					edge.transition_graph()?.map(Cow::into_owned),
				)?
			}
		};
		if ids.insert(source_id, local_id).is_some() {
			return Err(FerrosGrynnError::invalid_storage_file(
				"primary index contains a duplicate source atom",
			));
		}
		source_atoms.insert(local_id, source_ref);
		record_atom(source_id);
		Ok(())
	})?;

	visit_local_primary(store, &view.transaction, &view.graph, |key, disk| {
		if disk.kind() != AtomKind::Edge {
			return Ok(());
		}
		let record_id = decode_record_key(key, &context)?;
		let source_id = local_atom_id(store, &view.transaction, &view.graph, disk)?;
		let local_id = ids.get(&source_id).copied().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"edge identity was not reserved during materialization",
			)
		})?;
		let edge =
			read_local_edge(store, &view.transaction, &view.graph, source_id, record_id, None)?
				.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"primary index references a missing edge",
					)
				})?;
		let topology = remap_topology(edge.topology()?.as_ref(), &ids)?;
		result.materialize_query_edge_topology(local_id, topology)
	})?;

	if ids.len() != node_count.saturating_add(edge_count) {
		return Err(FerrosGrynnError::invalid_storage_file(
			"primary index live-atom count disagrees with the graph superblock",
		));
	}
	Ok((result, source_atoms))
}

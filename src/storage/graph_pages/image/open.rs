//! Validation and restoration of one immutable graph-occurrence generation.

use std::collections::{BTreeMap, BTreeSet};

use super::{
	BuiltGraphImage,
	GraphArenaBindings,
	decode_topology::decode_topology,
	readers::{copy_page, local_chain, subgraph_entries},
	topology::{NodeAdjacency, record_node_adjacency},
};
use crate::{
	datastore::{DatabaseRoot, GlobalAtomId, GraphRoot, OccurrenceState},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, AtomKind, GraphId},
	storage::{
		graph_index::{LabelDictionary, LabelKind, decode_label_dictionary, scan_primary_index},
		graph_pages::{
			EdgeHotSlot,
			EdgeSlotFlags,
			HOT_SLOTS_PER_PAGE,
			NodeHotSlot,
			NodeSlotFlags,
			hot_slot_address,
			read_edge_hot_slot,
			read_node_hot_slot,
			read_overlay_record,
		},
		graph_superblock::GraphSuperblock,
		page::{AlignedPage, PageNo},
	},
	values::{
		Number,
		Text,
		storage::{DecodeContext, DiskAtomRef, codec::DecodeLimits},
	},
};

/// Reopens a complete graph image after validating every cross-file relation.
pub(in crate::storage) fn open_graph_image(
	image: BuiltGraphImage,
	database: &DatabaseRoot,
	context: &DecodeContext<'_>,
) -> FerrosGrynnResult<GraphRoot> {
	validate_ceilings(&image)?;
	let metadata = GraphSuperblock::decode(&image.superblock.encode()?)?;
	if metadata.graph_id != image.reference.id() {
		return Err(invalid("GraphSuperblock and GraphRef identities disagree"));
	}
	validate_arena_root(&image.arena, metadata.graph_id)?;
	let labels = decode_label_dictionary(|page_no, output| {
		copy_page(&image.labels, page_no, output, "label dictionary")
	})?;
	let memberships = decode_memberships(&image)?;
	let nodes = decode_nodes(&image, metadata.node_capacity)?;
	let edges = decode_edges(&image, metadata.edge_capacity)?;
	let locals = local_addresses(&nodes, &edges)?;
	if locals.len() != memberships.len() {
		return Err(invalid("membership index count disagrees with live hot slots"));
	}

	let mut graph = GraphRoot::new(image.reference.clone());
	let mut remaining = memberships;
	let mut expected_adjacency = BTreeMap::new();
	for (slot_index, slot) in nodes.into_iter().enumerate() {
		let id = local_id(AtomKind::Node, slot_index, slot.generation)?;
		if slot.flags.contains(NodeSlotFlags::TOMBSTONE) {
			graph.restore_free_slot(AtomKind::Node, id.slot(), id.generation())?;
			continue;
		}
		let global = validate_membership(database, &mut remaining, id, slot.global_atom)?;
		let nested_graph = child_graph(slot.subgraph_slot, &image.arena)?;
		graph.attach_node_at(
			id,
			global,
			label(&labels, LabelKind::Node, slot.label_id)?,
			nested_graph,
		)?;
		install_overlay(&mut graph, id, &image.overlays, slot.overlay, context)?;
		expected_adjacency.insert(id, NodeAdjacency {
			outgoing: read_local_side(
				slot.out_count,
				&slot.out_inline,
				slot.out_overflow,
				&image.adjacency,
				&locals,
				AtomKind::Edge,
			)?,
			incoming: read_local_side(
				slot.in_count,
				&slot.in_inline,
				slot.in_overflow,
				&image.adjacency,
				&locals,
				AtomKind::Edge,
			)?,
			undirected: read_local_side(
				slot.undirected_count,
				&slot.undirected_inline,
				slot.undirected_overflow,
				&image.adjacency,
				&locals,
				AtomKind::Edge,
			)?,
		});
	}

	for (slot_index, slot) in edges.into_iter().enumerate() {
		let id = local_id(AtomKind::Edge, slot_index, slot.generation)?;
		if slot.flags.contains(EdgeSlotFlags::TOMBSTONE) {
			graph.restore_free_slot(AtomKind::Edge, id.slot(), id.generation())?;
			continue;
		}
		let global = validate_membership(database, &mut remaining, id, slot.global_atom)?;
		let topology = decode_topology(
			&slot,
			&image.adjacency,
			&image.cross_level_adjacency,
			&locals,
			&image.arena,
			database,
		)?;
		let transition = child_graph(slot.transition_subgraph_slot, &image.arena)?;
		graph.restore_edge_at(
			id,
			global,
			label(&labels, LabelKind::Edge, slot.label_id)?,
			topology,
			slot.weight.map(Number::from),
			transition,
		)?;
		install_overlay(&mut graph, id, &image.overlays, slot.overlay, context)?;
	}
	if !remaining.is_empty() {
		return Err(invalid("membership index references absent hot slots"));
	}
	graph.validate_restored_endpoints()?;
	validate_node_adjacency(&graph, expected_adjacency)?;
	validate_subgraphs(&image, database, &graph)?;
	Ok(graph)
}

fn decode_nodes(image: &BuiltGraphImage, capacity: u64) -> FerrosGrynnResult<Vec<NodeHotSlot>> {
	decode_hot(capacity, &image.nodes, |page, page_no, slot| {
		read_node_hot_slot(page, page_no, slot)
	})
}

fn decode_edges(image: &BuiltGraphImage, capacity: u64) -> FerrosGrynnResult<Vec<EdgeHotSlot>> {
	decode_hot(capacity, &image.edges, |page, page_no, slot| {
		read_edge_hot_slot(page, page_no, slot)
	})
}

fn decode_hot<Slot>(
	capacity: u64,
	pages: &[AlignedPage],
	mut decode: impl FnMut(&AlignedPage, PageNo, usize) -> FerrosGrynnResult<Slot>,
) -> FerrosGrynnResult<Vec<Slot>> {
	let capacity =
		usize::try_from(capacity).map_err(|_error| invalid("hot capacity exceeds usize"))?;
	let expected_pages = capacity.div_ceil(HOT_SLOTS_PER_PAGE);
	if pages.len() != expected_pages {
		return Err(invalid("hot page count disagrees with its capacity"));
	}
	let mut result = Vec::with_capacity(capacity);
	for slot in 0..capacity {
		let (page_no, within) = hot_slot_address(
			u64::try_from(slot).map_err(|_error| invalid("hot slot exceeds u64"))?,
		)?;
		let page_index = usize::try_from(page_no.get())
			.map_err(|_error| invalid("hot page number exceeds usize"))?;
		result.push(decode(
			pages.get(page_index).ok_or_else(|| invalid("hot page is absent"))?,
			page_no,
			within,
		)?);
	}
	Ok(result)
}

fn local_addresses(
	nodes: &[NodeHotSlot],
	edges: &[EdgeHotSlot],
) -> FerrosGrynnResult<BTreeMap<DiskAtomRef, AtomId>> {
	let mut result = BTreeMap::new();
	for (kind, generations) in [
		(
			AtomKind::Node,
			nodes
				.iter()
				.map(|slot| (slot.generation, slot.flags.contains(NodeSlotFlags::TOMBSTONE)))
				.collect::<Vec<_>>(),
		),
		(
			AtomKind::Edge,
			edges
				.iter()
				.map(|slot| (slot.generation, slot.flags.contains(EdgeSlotFlags::TOMBSTONE)))
				.collect::<Vec<_>>(),
		),
	] {
		for (slot, (generation, tombstone)) in generations.into_iter().enumerate() {
			if tombstone {
				continue;
			}
			let id = local_id(kind, slot, generation)?;
			let disk = DiskAtomRef::from_parts(kind, id.slot())?;
			if result.insert(disk, id).is_some() {
				return Err(invalid("live hot slots have a duplicate physical address"));
			}
		}
	}
	Ok(result)
}

fn decode_memberships(
	image: &BuiltGraphImage,
) -> FerrosGrynnResult<BTreeMap<DiskAtomRef, GlobalAtomId>> {
	let mut result = BTreeMap::new();
	for entry in scan_primary_index::<DiskAtomRef>(
		|page_no, output| copy_page(&image.membership.pages, page_no, output, "membership index"),
		image.membership.root,
	)? {
		let bytes: [u8; 8] = entry
			.key
			.as_slice()
			.try_into()
			.map_err(|_error| invalid("membership key is not exactly eight bytes"))?;
		let global = GlobalAtomId::from_storage(u64::from_be_bytes(bytes))?;
		if result.insert(entry.atom, global).is_some() {
			return Err(invalid("membership index contains a duplicate local address"));
		}
	}
	Ok(result)
}

fn validate_membership(
	database: &DatabaseRoot,
	remaining: &mut BTreeMap<DiskAtomRef, GlobalAtomId>,
	local: AtomId,
	stored: Option<GlobalAtomId>,
) -> FerrosGrynnResult<GlobalAtomId> {
	let stored = stored.ok_or_else(|| invalid("live hot slot has no global atom"))?;
	let key = DiskAtomRef::from_parts(
		local.kind().ok_or_else(|| invalid("live local identity is null"))?,
		local.slot(),
	)?;
	let indexed = remaining
		.remove(&key)
		.ok_or_else(|| invalid("live hot slot is absent from membership index"))?;
	if indexed != stored || indexed.kind() != local.kind() || database.atom(indexed).is_none() {
		return Err(invalid("hot slot, membership index, and AtomStore disagree"));
	}
	Ok(indexed)
}

fn install_overlay(
	graph: &mut GraphRoot,
	local: AtomId,
	pages: &[AlignedPage],
	reference: crate::storage::atom_page::PropertyRef,
	context: &DecodeContext<'_>,
) -> FerrosGrynnResult<()> {
	let Some(record) =
		read_overlay_record(pages, reference, DecodeLimits::default().max_total_allocation)?
	else {
		return Ok(());
	};
	let overlay =
		crate::storage::graph_pages::decode_overlay(&record, context, DecodeLimits::default())?;
	graph
		.occurrence_mut(local)
		.ok_or_else(|| invalid("restored occurrence disappeared before overlay installation"))?
		.replace_overlay(Some(overlay));
	Ok(())
}

fn read_local_side<const N: usize>(
	count: u32,
	inline: &[Option<DiskAtomRef>; N],
	overflow: Option<PageNo>,
	pages: &[AlignedPage],
	locals: &BTreeMap<DiskAtomRef, AtomId>,
	expected_kind: AtomKind,
) -> FerrosGrynnResult<Vec<DiskAtomRef>> {
	let count =
		usize::try_from(count).map_err(|_error| invalid("adjacency count exceeds usize"))?;
	let mut result = inline.iter().take(count.min(N)).copied().flatten().collect::<Vec<_>>();
	result.extend(local_chain(pages, overflow)?);
	if result.len() != count
		|| result
			.iter()
			.any(|reference| reference.kind() != expected_kind || !locals.contains_key(reference))
	{
		return Err(invalid("node adjacency count or live edge binding is invalid"));
	}
	Ok(result)
}

fn validate_node_adjacency(
	graph: &GraphRoot,
	expected: BTreeMap<AtomId, NodeAdjacency>,
) -> FerrosGrynnResult<()> {
	let mut actual = graph
		.occurrences()
		.filter(|occurrence| occurrence.local_id().kind() == Some(AtomKind::Node))
		.map(|occurrence| (occurrence.local_id(), NodeAdjacency::default()))
		.collect::<BTreeMap<_, _>>();
	for occurrence in graph.occurrences() {
		if let OccurrenceState::Edge {
			topology,
			..
		} = occurrence.state()
		{
			record_node_adjacency(
				topology,
				DiskAtomRef::from_parts(AtomKind::Edge, occurrence.local_id().slot())?,
				&mut actual,
			)?;
		}
	}
	if actual != expected {
		return Err(invalid("NodeHot adjacency disagrees with edge topology"));
	}
	Ok(())
}

fn validate_subgraphs(
	image: &BuiltGraphImage,
	database: &DatabaseRoot,
	graph: &GraphRoot,
) -> FerrosGrynnResult<()> {
	let entries = subgraph_entries(&image.subgraphs)?;
	if u64::try_from(entries.len()).ok() != Some(image.superblock.subgraph_count) {
		return Err(invalid("subgraph directory count disagrees with GraphSuperblock"));
	}
	let mut seen = BTreeSet::new();
	for entry in entries {
		if entry.slot == 0 || !seen.insert(entry.slot) {
			return Err(invalid("subgraph directory contains a reserved or duplicate slot"));
		}
		let scope = image
			.arena
			.scope_by_slot
			.get(&entry.slot)
			.ok_or_else(|| invalid("subgraph slot has no catalog scope binding"))?;
		if scope.len() != 1 || scope[0].generation() != entry.generation {
			return Err(invalid("direct subgraph scope or container generation disagrees"));
		}
		let child_id = image
			.arena
			.graph_by_slot
			.get(&entry.slot)
			.copied()
			.ok_or_else(|| invalid("subgraph slot has no graph binding"))?;
		let child = database
			.graph(child_id)
			.ok_or_else(|| invalid("subgraph directory references an absent graph"))?;
		if entry.node_capacity != usize_u64(child.slot_capacity(AtomKind::Node))?
			|| entry.edge_capacity != usize_u64(child.slot_capacity(AtomKind::Edge))?
			|| entry.node_count
				!= usize_u32(
					child
						.occurrences()
						.filter(|occurrence| occurrence.local_id().kind() == Some(AtomKind::Node))
						.count(),
				)? || entry.edge_count
			!= usize_u32(
				child
					.occurrences()
					.filter(|occurrence| occurrence.local_id().kind() == Some(AtomKind::Edge))
					.count(),
			)? {
			return Err(invalid("subgraph directory child capacity or count disagrees"));
		}
		let container = graph
			.occurrence(scope[0])
			.ok_or_else(|| invalid("subgraph container is absent from parent graph"))?;
		let bound = match container.state() {
			OccurrenceState::Node {
				nested_graph,
			} => *nested_graph,
			OccurrenceState::Edge {
				transition_graph,
				..
			} => *transition_graph,
		};
		if bound != Some(child_id) {
			return Err(invalid("subgraph catalog binding disagrees with its container"));
		}
	}
	let direct = image
		.arena
		.scope_by_slot
		.iter()
		.filter(|(slot, scope)| **slot != 0 && scope.len() == 1)
		.count();
	if direct != seen.len() {
		return Err(invalid("catalog contains a direct subgraph absent from its directory"));
	}
	Ok(())
}

fn validate_ceilings(image: &BuiltGraphImage) -> FerrosGrynnResult<()> {
	for (actual, expected, family) in [
		(image.nodes.len(), image.superblock.node_hot_pages, "node-hot"),
		(image.edges.len(), image.superblock.edge_hot_pages, "edge-hot"),
		(image.adjacency.len(), image.superblock.adjacency_overflow_pages, "adjacency"),
		(
			image.cross_level_adjacency.len(),
			image.superblock.cross_level_adjacency_overflow_pages,
			"cross-level adjacency",
		),
		(image.overlays.len(), image.superblock.overlays_pages, "overlays"),
		(image.subgraphs.len(), image.superblock.subgraphs_pages, "subgraphs"),
		(image.labels.len(), image.superblock.labels_pages, "labels"),
	] {
		if usize_u64(actual)? != expected {
			return Err(invalid(format!("{family} page count disagrees with GraphSuperblock")));
		}
	}
	Ok(())
}

fn validate_arena_root(arena: &GraphArenaBindings, graph: GraphId) -> FerrosGrynnResult<()> {
	if arena.graph_by_slot.get(&0) != Some(&graph)
		|| arena.scope_by_slot.get(&0).is_none_or(|scope| !scope.is_empty())
		|| arena.graph_by_slot.len() != arena.scope_by_slot.len()
	{
		return Err(invalid("root-arena catalog binding is incomplete"));
	}
	Ok(())
}

fn child_graph(
	slot: Option<u32>,
	arena: &GraphArenaBindings,
) -> FerrosGrynnResult<Option<GraphId>> {
	slot.map(|slot| {
		if slot == 0 {
			return Err(invalid("nested graph uses reserved root-arena slot zero"));
		}
		arena
			.graph_by_slot
			.get(&slot)
			.copied()
			.ok_or_else(|| invalid("nested graph uses an unknown root-arena slot"))
	})
	.transpose()
}

fn label(
	dictionary: &LabelDictionary,
	kind: LabelKind,
	id: Option<u32>,
) -> FerrosGrynnResult<Option<Text>> {
	id.map(|id| {
		dictionary
			.name(kind, id)
			.map(Text::from)
			.ok_or_else(|| invalid("hot slot label ID exceeds its namespace"))
	})
	.transpose()
}

fn local_id(kind: AtomKind, slot: usize, generation: u32) -> FerrosGrynnResult<AtomId> {
	AtomId::from_parts(
		kind,
		u64::try_from(slot).map_err(|_error| invalid("hot slot exceeds u64"))?,
		generation,
	)
}

fn usize_u64(value: usize) -> FerrosGrynnResult<u64> {
	u64::try_from(value).map_err(|_error| invalid("value exceeds u64"))
}

fn usize_u32(value: usize) -> FerrosGrynnResult<u32> {
	u32::try_from(value).map_err(|_error| invalid("value exceeds u32"))
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

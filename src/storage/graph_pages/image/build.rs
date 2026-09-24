//! Construction of one immutable graph-occurrence generation.

use std::{collections::BTreeMap, path::Path};

use super::{
	BuiltGraphImage,
	arena::GraphArena,
	builders::{
		OverflowBuilder,
		QualifiedOverflowBuilder,
		inline_refs,
		page_index,
		page_no,
		subgraph_pages,
	},
	topology::{NodeAdjacency, encode_hot_side, encode_topology, record_node_adjacency},
};
use crate::{
	datastore::{DatabaseRoot, GraphRoot, OccurrenceState},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, AtomKind, GraphId},
	storage::{
		graph_index::{LabelDictionary, LabelKind, SortedIndexRecords, encode_label_dictionary},
		graph_pages::{
			EdgeHotSlot,
			EdgeSlotFlags,
			GraphOverlayStore,
			HOT_SLOTS_PER_PAGE,
			NodeHotSlot,
			NodeSlotFlags,
			PageLsn,
			encode_overlay,
			hot_slot_address,
			initialize_edge_hot_page,
			initialize_node_hot_page,
			write_edge_hot_slot,
			write_node_hot_slot,
		},
		graph_superblock::GraphSuperblock,
		membership_index::{build_graph_membership_index, sort_graph_membership_index},
		page::AlignedPage,
		types::{CheckpointGeneration, Lsn},
	},
	values::storage::{DiskAtomRef, EncodeContext},
};

/// Builds every graph-local page family without publishing any file.
pub(in crate::storage) fn build_graph_image(
	database: &DatabaseRoot,
	graph_id: GraphId,
	context: &mut EncodeContext,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<BuiltGraphImage> {
	build_graph_image_with_membership(database, graph_id, context, page_lsn, |graph| {
		build_graph_membership_index(graph, page_lsn)
	})
}

/// Builds graph payload plus bounded membership input for an import generation.
pub(in crate::storage) fn build_import_graph_image(
	database: &DatabaseRoot,
	graph_id: GraphId,
	context: &mut EncodeContext,
	page_lsn: PageLsn,
	scratch_root: &Path,
) -> FerrosGrynnResult<BuiltGraphImage<SortedIndexRecords>> {
	build_graph_image_with_membership(database, graph_id, context, page_lsn, |graph| {
		sort_graph_membership_index(graph, scratch_root)
	})
}

fn build_graph_image_with_membership<Membership>(
	database: &DatabaseRoot,
	graph_id: GraphId,
	context: &mut EncodeContext,
	page_lsn: PageLsn,
	build_membership: impl FnOnce(&GraphRoot) -> FerrosGrynnResult<Membership>,
) -> FerrosGrynnResult<BuiltGraphImage<Membership>> {
	let graph = database
		.graph(graph_id)
		.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph_id.to_string()))?;
	let arena = GraphArena::build(database, graph_id)?;
	let labels = LabelDictionary::from_labels(graph.occurrences().filter_map(|occurrence| {
		occurrence.label().map(|label| (label_kind(occurrence.local_id()), label.as_str()))
	}))?;
	let label_pages = encode_label_dictionary(&labels, page_lsn)?;

	let mut locals = BTreeMap::new();
	let mut node_adjacency = BTreeMap::new();
	for occurrence in graph.occurrences() {
		let atom = database
			.atom(occurrence.global_id())
			.ok_or_else(|| invalid("graph occurrence references an absent global atom"))?;
		if atom.kind() != occurrence_kind(occurrence.local_id())? {
			return Err(invalid("graph occurrence and global atom kinds disagree"));
		}
		let local = disk_ref(occurrence.local_id())?;
		if locals.insert(occurrence.local_id(), local).is_some() {
			return Err(invalid("graph contains a duplicate local occurrence"));
		}
		if occurrence.local_id().kind() == Some(AtomKind::Node) {
			node_adjacency.insert(occurrence.local_id(), NodeAdjacency::default());
		}
	}
	for occurrence in graph.occurrences() {
		if let OccurrenceState::Edge {
			topology,
			..
		} = occurrence.state()
		{
			record_node_adjacency(topology, disk_ref(occurrence.local_id())?, &mut node_adjacency)?;
		}
	}

	let node_page_count = graph.slot_capacity(AtomKind::Node).div_ceil(HOT_SLOTS_PER_PAGE);
	// Allocate every final frame before writing a valid page. This avoids
	// moving populated, 16 KiB-aligned values through the Windows MSVC ASan
	// codegen-sensitive path described in `docs/reports/
	// aligned_page_corruption_investigation.md`.
	let mut nodes = (0..node_page_count).map(|_| AlignedPage::zeroed()).collect::<Vec<_>>();
	for (index, page) in nodes.iter_mut().enumerate() {
		initialize_node_hot_page(page, page_no(index, "node hot")?, page_lsn)?;
	}
	let edge_page_count = graph.slot_capacity(AtomKind::Edge).div_ceil(HOT_SLOTS_PER_PAGE);
	let mut edges = (0..edge_page_count).map(|_| AlignedPage::zeroed()).collect::<Vec<_>>();
	for (index, page) in edges.iter_mut().enumerate() {
		initialize_edge_hot_page(page, page_no(index, "edge hot")?, page_lsn)?;
	}
	write_tombstone_generations(graph, &mut nodes, &mut edges)?;

	let mut overlays = GraphOverlayStore::default();
	let mut adjacency = OverflowBuilder::default();
	let mut qualified_adjacency = QualifiedOverflowBuilder::default();
	for occurrence in graph.occurrences() {
		let overlay = occurrence
			.overlay()
			.map(|overlay| {
				let record = encode_overlay(overlay, context)?;
				overlays.insert(&record, page_lsn)
			})
			.transpose()?
			.unwrap_or(crate::storage::atom_page::PropertyRef::None);
		let label = occurrence
			.label()
			.map(|label| {
				labels
					.id(label_kind(occurrence.local_id()), label.as_str())
					.ok_or_else(|| invalid("graph label is absent from its dictionary"))
			})
			.transpose()?;
		match occurrence.state() {
			OccurrenceState::Node {
				nested_graph,
			} => {
				let local = occurrence.local_id();
				let incident = node_adjacency
					.get(&local)
					.ok_or_else(|| invalid("live node has no adjacency accumulator"))?;
				let out_overflow =
					adjacency.append(incident.outgoing.get(4..).unwrap_or(&[]), page_lsn)?;
				let in_overflow =
					adjacency.append(incident.incoming.get(4..).unwrap_or(&[]), page_lsn)?;
				let undirected_overflow =
					adjacency.append(incident.undirected.get(2..).unwrap_or(&[]), page_lsn)?;
				let subgraph_slot = nested_graph
					.map(|child| {
						arena
							.direct_slot(graph_id, child)
							.ok_or_else(|| invalid("nested graph has no direct arena binding"))
					})
					.transpose()?;
				let mut flags = NodeSlotFlags::empty();
				flags.set(NodeSlotFlags::META, subgraph_slot.is_some());
				flags.set(NodeSlotFlags::HAS_OVERLAY, occurrence.has_overlay());
				write_node(&mut nodes, local, &NodeHotSlot {
					generation: local.generation(),
					subgraph_slot,
					label_id: label,
					out_count: u32_len(incident.outgoing.len(), "node outgoing")?,
					in_count: u32_len(incident.incoming.len(), "node incoming")?,
					undirected_count: u32_len(incident.undirected.len(), "node undirected")?,
					flags,
					out_overflow,
					in_overflow,
					undirected_overflow,
					overlay,
					global_atom: Some(occurrence.global_id()),
					out_inline: inline_refs(&incident.outgoing),
					in_inline: inline_refs(&incident.incoming),
					undirected_inline: inline_refs(&incident.undirected),
				})?;
			}
			OccurrenceState::Edge {
				topology,
				weight,
				transition_graph,
			} => {
				let encoded = encode_topology(topology, &locals, &arena)?;
				let cross_level = topology.is_cross_level();
				let source = encode_hot_side(
					&encoded.source,
					crate::storage::graph_pages::EndpointSide::Source,
					cross_level,
					page_lsn,
					&mut adjacency,
					&mut qualified_adjacency,
				)?;
				let target = encode_hot_side(
					&encoded.target,
					crate::storage::graph_pages::EndpointSide::Target,
					cross_level,
					page_lsn,
					&mut adjacency,
					&mut qualified_adjacency,
				)?;
				let transition_subgraph_slot = transition_graph
					.map(|child| {
						arena
							.direct_slot(graph_id, child)
							.ok_or_else(|| invalid("transition graph has no direct arena binding"))
					})
					.transpose()?;
				let mut flags = EdgeSlotFlags::empty();
				flags.set(EdgeSlotFlags::META, transition_subgraph_slot.is_some());
				flags.set(EdgeSlotFlags::CROSS_LEVEL, cross_level);
				flags.set(EdgeSlotFlags::HAS_OVERLAY, occurrence.has_overlay());
				write_edge(&mut edges, occurrence.local_id(), &EdgeHotSlot {
					topology: encoded.code,
					flags,
					has_transition: transition_subgraph_slot.is_some(),
					generation: occurrence.local_id().generation(),
					source_count: u32_len(encoded.source.len(), "edge source")?,
					target_count: u32_len(encoded.target.len(), "edge target")?,
					label_id: label,
					weight: weight.map(f64::try_from).transpose()?,
					transition_subgraph_slot,
					source_subgraph_slot: source.graph_slot,
					target_subgraph_slot: target.graph_slot,
					overlay,
					source_overflow: source.local_overflow,
					target_overflow: target.local_overflow,
					global_atom: Some(occurrence.global_id()),
					source_subgraph_overflow: source.graph_overflow,
					target_subgraph_overflow: target.graph_overflow,
					source_inline: source.inline,
					target_inline: target.inline,
				})?;
			}
		}
	}
	seal_all(&mut nodes)?;
	seal_all(&mut edges)?;

	let adjacency = adjacency.finish();
	let cross_level_adjacency = qualified_adjacency.finish();
	let overlays = overlays.finish()?;
	let subgraph_entries = arena.entries(graph_id)?;
	let subgraphs = subgraph_pages(&subgraph_entries, page_lsn)?;
	let membership = build_membership(graph)?;
	let node_count = graph
		.occurrences()
		.filter(|occurrence| occurrence.local_id().kind() == Some(AtomKind::Node))
		.count();
	let edge_count = graph.occurrences().len() - node_count;
	let superblock = GraphSuperblock {
		page_lsn,
		graph_id,
		checkpoint_generation: CheckpointGeneration::ZERO,
		backfilled_lsn: Lsn::ZERO,
		checkpoint_record_lsn: Lsn::ZERO,
		node_capacity: usize_u64(graph.slot_capacity(AtomKind::Node), "node capacity")?,
		edge_capacity: usize_u64(graph.slot_capacity(AtomKind::Edge), "edge capacity")?,
		node_count: usize_u64(node_count, "node count")?,
		edge_count: usize_u64(edge_count, "edge count")?,
		subgraph_capacity: usize_u64(subgraph_entries.len(), "subgraph capacity")?,
		subgraph_count: usize_u64(subgraph_entries.len(), "subgraph count")?,
		node_hot_pages: usize_u64(nodes.len(), "node-hot pages")?,
		edge_hot_pages: usize_u64(edges.len(), "edge-hot pages")?,
		adjacency_overflow_pages: usize_u64(adjacency.len(), "adjacency pages")?,
		cross_level_adjacency_overflow_pages: usize_u64(
			cross_level_adjacency.len(),
			"cross-level pages",
		)?,
		overlays_pages: usize_u64(overlays.len(), "overlay pages")?,
		subgraphs_pages: usize_u64(subgraphs.len(), "subgraph pages")?,
		labels_pages: usize_u64(label_pages.len(), "label pages")?,
	};
	// Validate the complete metadata relation before the image can be
	// published.
	let _page = superblock.encode()?;

	Ok(BuiltGraphImage {
		reference: graph.reference().clone(),
		superblock,
		nodes,
		edges,
		adjacency,
		cross_level_adjacency,
		overlays,
		subgraphs,
		labels: label_pages,
		membership,
		arena: arena.bindings(),
	})
}

fn write_tombstone_generations(
	graph: &crate::datastore::GraphRoot,
	nodes: &mut [crate::storage::page::AlignedPage],
	edges: &mut [crate::storage::page::AlignedPage],
) -> FerrosGrynnResult<()> {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		for slot in 0..graph.slot_capacity(kind) {
			let generation = graph
				.slot_generation(kind, slot)
				.ok_or_else(|| invalid("graph allocator has a missing generation"))?;
			let slot = u64::try_from(slot).map_err(|_error| invalid("graph slot exceeds u64"))?;
			let id = AtomId::from_parts(kind, slot, generation)?;
			match kind {
				AtomKind::Node => write_node(nodes, id, &NodeHotSlot::tombstone(generation))?,
				AtomKind::Edge => write_edge(edges, id, &EdgeHotSlot::tombstone(generation))?,
			}
		}
	}
	Ok(())
}

fn write_node(
	pages: &mut [crate::storage::page::AlignedPage],
	id: AtomId,
	value: &NodeHotSlot,
) -> FerrosGrynnResult<()> {
	let (page, slot) = hot_slot_address(id.slot())?;
	let index = page_index(page, pages.len())?;
	write_node_hot_slot(&mut pages[index], slot, value)
}

fn write_edge(
	pages: &mut [crate::storage::page::AlignedPage],
	id: AtomId,
	value: &EdgeHotSlot,
) -> FerrosGrynnResult<()> {
	let (page, slot) = hot_slot_address(id.slot())?;
	let index = page_index(page, pages.len())?;
	write_edge_hot_slot(&mut pages[index], slot, value)
}

fn seal_all(pages: &mut [crate::storage::page::AlignedPage]) -> FerrosGrynnResult<()> {
	for page in pages {
		page.seal()?;
	}
	Ok(())
}

fn label_kind(id: AtomId) -> LabelKind {
	match id.kind() {
		Some(AtomKind::Node) => LabelKind::Node,
		Some(AtomKind::Edge) | None => LabelKind::Edge,
	}
}

fn occurrence_kind(id: AtomId) -> FerrosGrynnResult<AtomKind> {
	id.kind().ok_or_else(|| invalid("graph occurrence uses null identity"))
}

fn disk_ref(id: AtomId) -> FerrosGrynnResult<DiskAtomRef> {
	DiskAtomRef::from_parts(occurrence_kind(id)?, id.slot())
}

fn u32_len(value: usize, field: &str) -> FerrosGrynnResult<u32> {
	u32::try_from(value).map_err(|_error| invalid(format!("{field} exceeds u32")))
}

fn usize_u64(value: usize, field: &str) -> FerrosGrynnResult<u64> {
	u64::try_from(value).map_err(|_error| invalid(format!("{field} exceeds u64")))
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

//! Restoration of runtime topology from local and root-arena addresses.

use std::collections::BTreeMap;

use super::{
	GraphArenaBindings,
	readers::{local_chain, qualified_chain},
};
use crate::{
	datastore::DatabaseRoot,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, EdgeEndpoint, EdgeTopology, QualifiedAtomId},
	storage::{
		graph_pages::{
			EdgeHotSlot,
			EdgeSlotFlags,
			EdgeTopologyCode,
			EndpointSide,
			QualifiedDiskAtomRef,
		},
		page::AlignedPage,
	},
	values::storage::DiskAtomRef,
};

pub(super) fn decode_topology(
	slot: &EdgeHotSlot,
	adjacency: &[AlignedPage],
	cross_level: &[AlignedPage],
	locals: &BTreeMap<DiskAtomRef, AtomId>,
	arena: &GraphArenaBindings,
	database: &DatabaseRoot,
) -> FerrosGrynnResult<EdgeTopology> {
	let (source, target) = if slot.flags.contains(EdgeSlotFlags::CROSS_LEVEL) {
		(
			cross_side(
				slot.source_count,
				&slot.source_inline,
				slot.source_subgraph_slot,
				slot.source_subgraph_overflow,
				EndpointSide::Source,
				cross_level,
				locals,
				arena,
				database,
			)?,
			cross_side(
				slot.target_count,
				&slot.target_inline,
				slot.target_subgraph_slot,
				slot.target_subgraph_overflow,
				EndpointSide::Target,
				cross_level,
				locals,
				arena,
				database,
			)?,
		)
	} else {
		(
			local_side(
				slot.source_count,
				&slot.source_inline,
				slot.source_overflow,
				adjacency,
				locals,
			)?,
			local_side(
				slot.target_count,
				&slot.target_inline,
				slot.target_overflow,
				adjacency,
				locals,
			)?,
		)
	};

	Ok(match slot.topology {
		EdgeTopologyCode::Directed | EdgeTopologyCode::HyperDirected | EdgeTopologyCode::Loop => {
			EdgeTopology::directed(source, target)
		}
		EdgeTopologyCode::Bidirectional => EdgeTopology::bidirectional(source, target),
		EdgeTopologyCode::Undirected | EdgeTopologyCode::HyperUndirected => {
			if !target.is_empty() {
				return Err(invalid("undirected topology contains target endpoints"));
			}
			EdgeTopology::undirected(source)
		}
	})
}

fn local_side<const N: usize>(
	count: u32,
	inline: &[Option<DiskAtomRef>; N],
	overflow: Option<crate::storage::page::PageNo>,
	pages: &[AlignedPage],
	locals: &BTreeMap<DiskAtomRef, AtomId>,
) -> FerrosGrynnResult<Vec<EdgeEndpoint>> {
	let count = usize::try_from(count).map_err(|_error| invalid("endpoint count exceeds usize"))?;
	let mut encoded = inline.iter().take(count.min(N)).copied().flatten().collect::<Vec<_>>();
	encoded.extend(local_chain(pages, overflow)?);
	if encoded.len() != count {
		return Err(invalid("local endpoint count disagrees with its storage"));
	}
	encoded
		.into_iter()
		.map(|reference| {
			locals
				.get(&reference)
				.copied()
				.map(EdgeEndpoint::Local)
				.ok_or_else(|| invalid("local endpoint references a tombstone"))
		})
		.collect()
}

#[allow(clippy::too_many_arguments, reason = "the exact hot-side fields form one decode contract")]
fn cross_side<const N: usize>(
	count: u32,
	inline: &[Option<DiskAtomRef>; N],
	graph_slot: Option<u32>,
	overflow: Option<crate::storage::page::PageNo>,
	side: EndpointSide,
	pages: &[AlignedPage],
	locals: &BTreeMap<DiskAtomRef, AtomId>,
	arena: &GraphArenaBindings,
	database: &DatabaseRoot,
) -> FerrosGrynnResult<Vec<EdgeEndpoint>> {
	let count = usize::try_from(count).map_err(|_error| invalid("endpoint count exceeds usize"))?;
	if count == 0 {
		return Ok(Vec::new());
	}
	let encoded = if let Some(first) = overflow {
		qualified_chain(pages, Some(first), side)?
	} else {
		let slot = graph_slot.ok_or_else(|| invalid("inline qualified side has no graph slot"))?;
		inline
			.iter()
			.take(count.min(N))
			.copied()
			.flatten()
			.map(|atom| QualifiedDiskAtomRef {
				graph_slot: slot,
				side,
				atom,
			})
			.collect()
	};
	if encoded.len() != count {
		return Err(invalid("qualified endpoint count disagrees with its storage"));
	}
	encoded.into_iter().map(|entry| resolve_qualified(entry, locals, arena, database)).collect()
}

fn resolve_qualified(
	entry: QualifiedDiskAtomRef,
	locals: &BTreeMap<DiskAtomRef, AtomId>,
	arena: &GraphArenaBindings,
	database: &DatabaseRoot,
) -> FerrosGrynnResult<EdgeEndpoint> {
	if entry.graph_slot == 0 {
		return locals
			.get(&entry.atom)
			.copied()
			.map(EdgeEndpoint::Local)
			.ok_or_else(|| invalid("qualified local endpoint references a tombstone"));
	}
	let graph_id = arena
		.graph_by_slot
		.get(&entry.graph_slot)
		.copied()
		.ok_or_else(|| invalid("qualified endpoint uses an unknown graph slot"))?;
	let scope = arena
		.scope_by_slot
		.get(&entry.graph_slot)
		.ok_or_else(|| invalid("qualified endpoint graph slot has no scope"))?;
	if scope.is_empty() {
		return Err(invalid("non-root qualified endpoint has an empty scope"));
	}
	let graph =
		database.graph(graph_id).ok_or_else(|| invalid("qualified endpoint graph is absent"))?;
	let slot = usize::try_from(entry.atom.slot())
		.map_err(|_error| invalid("qualified endpoint slot exceeds usize"))?;
	let generation = graph
		.slot_generation(entry.atom.kind(), slot)
		.ok_or_else(|| invalid("qualified endpoint slot exceeds graph capacity"))?;
	let atom = AtomId::from_parts(entry.atom.kind(), entry.atom.slot(), generation)?;
	if graph.occurrence(atom).is_none() {
		return Err(invalid("qualified endpoint references a tombstone"));
	}
	QualifiedAtomId::new(scope.to_vec(), atom).map(EdgeEndpoint::Nested)
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

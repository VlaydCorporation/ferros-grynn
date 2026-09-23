//! Conversion between runtime incidence and exact hot/overflow addresses.

use std::collections::BTreeMap;

use super::{arena::GraphArena, builders::*};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, AtomKind, EdgeEndpoint, EdgeTopology},
	storage::graph_pages::{EdgeTopologyCode, EndpointSide, PageLsn, PageNo},
	values::storage::DiskAtomRef,
};

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct NodeAdjacency {
	pub(super) outgoing: Vec<DiskAtomRef>,
	pub(super) incoming: Vec<DiskAtomRef>,
	pub(super) undirected: Vec<DiskAtomRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EncodedEndpoint {
	pub(super) graph_slot: u32,
	pub(super) atom: DiskAtomRef,
}

pub(super) struct EncodedTopology {
	pub(super) code: EdgeTopologyCode,
	pub(super) source: Vec<EncodedEndpoint>,
	pub(super) target: Vec<EncodedEndpoint>,
}

pub(super) struct EncodedHotSide {
	pub(super) inline: [Option<DiskAtomRef>; 2],
	pub(super) local_overflow: Option<PageNo>,
	pub(super) graph_slot: Option<u32>,
	pub(super) graph_overflow: Option<PageNo>,
}

pub(super) fn encode_topology(
	topology: &EdgeTopology,
	locals: &BTreeMap<AtomId, DiskAtomRef>,
	arena: &GraphArena<'_>,
) -> FerrosGrynnResult<EncodedTopology> {
	match topology {
		EdgeTopology::Directed {
			source,
			target,
		} => {
			let source = encode_endpoints(source, locals, arena)?;
			let target = encode_endpoints(target, locals, arena)?;
			let code = if source.len() == 1 && target.len() == 1 && source == target {
				EdgeTopologyCode::Loop
			} else if source.len() + target.len() == 2 {
				EdgeTopologyCode::Directed
			} else {
				EdgeTopologyCode::HyperDirected
			};
			Ok(EncodedTopology {
				code,
				source,
				target,
			})
		}
		EdgeTopology::Bidirectional {
			source,
			target,
		} => {
			let source = encode_endpoints(source, locals, arena)?;
			let target = encode_endpoints(target, locals, arena)?;
			if source.len() + target.len() != 2 {
				return Err(FerrosGrynnError::invalid_argument(
					"bidirectional topology requires exactly two endpoints in format v1",
				));
			}
			Ok(EncodedTopology {
				code: EdgeTopologyCode::Bidirectional,
				source,
				target,
			})
		}
		EdgeTopology::Undirected {
			endpoints,
		} => {
			let source = encode_endpoints(endpoints, locals, arena)?;
			Ok(EncodedTopology {
				code: if source.len() == 2 {
					EdgeTopologyCode::Undirected
				} else {
					EdgeTopologyCode::HyperUndirected
				},
				source,
				target: Vec::new(),
			})
		}
	}
}

fn encode_endpoints(
	endpoints: &[EdgeEndpoint],
	locals: &BTreeMap<AtomId, DiskAtomRef>,
	arena: &GraphArena<'_>,
) -> FerrosGrynnResult<Vec<EncodedEndpoint>> {
	endpoints
		.iter()
		.map(|endpoint| match endpoint {
			EdgeEndpoint::Local(id) => locals
				.get(id)
				.copied()
				.map(|atom| EncodedEndpoint {
					graph_slot: 0,
					atom,
				})
				.ok_or_else(|| invalid("edge endpoint references an absent local occurrence")),
			EdgeEndpoint::Nested(qualified) => {
				arena.qualified(qualified).map(|(graph_slot, atom)| EncodedEndpoint {
					graph_slot,
					atom,
				})
			}
		})
		.collect()
}

pub(super) fn encode_hot_side(
	entries: &[EncodedEndpoint],
	side: EndpointSide,
	cross_level: bool,
	page_lsn: PageLsn,
	local: &mut OverflowBuilder,
	qualified: &mut QualifiedOverflowBuilder,
) -> FerrosGrynnResult<EncodedHotSide> {
	if !cross_level {
		let atoms = entries.iter().map(|entry| entry.atom).collect::<Vec<_>>();
		return Ok(EncodedHotSide {
			inline: inline_refs(&atoms),
			local_overflow: local.append(atoms.get(2..).unwrap_or(&[]), page_lsn)?,
			graph_slot: None,
			graph_overflow: None,
		});
	}
	let one_graph = entries
		.first()
		.is_none_or(|first| entries.iter().all(|entry| entry.graph_slot == first.graph_slot));
	if entries.len() <= 2 && one_graph {
		let atoms = entries.iter().map(|entry| entry.atom).collect::<Vec<_>>();
		return Ok(EncodedHotSide {
			inline: inline_refs(&atoms),
			local_overflow: None,
			graph_slot: entries.first().map(|entry| entry.graph_slot),
			graph_overflow: None,
		});
	}
	Ok(EncodedHotSide {
		inline: [None; 2],
		local_overflow: None,
		graph_slot: None,
		graph_overflow: qualified.append(entries, side, page_lsn)?,
	})
}

pub(super) fn record_node_adjacency(
	topology: &EdgeTopology,
	edge: DiskAtomRef,
	nodes: &mut BTreeMap<AtomId, NodeAdjacency>,
) -> FerrosGrynnResult<()> {
	let mut append = |endpoint: &EdgeEndpoint,
	                  out: bool,
	                  input: bool,
	                  undirected: bool|
	 -> FerrosGrynnResult<()> {
		let Some(id) = endpoint.local_id() else {
			return Ok(());
		};
		if id.kind() != Some(AtomKind::Node) {
			return Ok(());
		}
		let adjacency = nodes
			.get_mut(&id)
			.ok_or_else(|| invalid("edge references an absent local node occurrence"))?;
		if out {
			adjacency.outgoing.push(edge);
		}
		if input {
			adjacency.incoming.push(edge);
		}
		if undirected {
			adjacency.undirected.push(edge);
		}
		Ok(())
	};
	match topology {
		EdgeTopology::Directed {
			source,
			target,
		} => {
			for endpoint in source {
				append(endpoint, true, false, false)?;
			}
			for endpoint in target {
				append(endpoint, false, true, false)?;
			}
		}
		EdgeTopology::Bidirectional {
			source,
			target,
		} => {
			for endpoint in source.iter().chain(target) {
				append(endpoint, true, true, false)?;
			}
		}
		EdgeTopology::Undirected {
			endpoints,
		} => {
			for endpoint in endpoints {
				append(endpoint, false, false, true)?;
			}
		}
	}
	Ok(())
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

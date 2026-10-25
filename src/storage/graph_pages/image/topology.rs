//! Conversion between runtime incidence and exact hot/overflow addresses.

use std::collections::BTreeMap;

use super::{
	arena::GraphArena,
	builders::{OverflowBuilder, QualifiedOverflowBuilder},
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, AtomKind, EdgeEndpoint, EdgeTopology},
	storage::graph_pages::{
		EncodedEndpoint,
		EncodedHotSide,
		EncodedTopology,
		EndpointSide,
		HotSideOverflow,
		PageLsn,
		plan_hot_side,
		resolve_topology,
	},
	values::storage::DiskAtomRef,
};

#[derive(Debug, Default, PartialEq, Eq)]
/// Node adjacency state.
pub(super) struct NodeAdjacency {
	/// Outgoing associated with this node adjacency.
	pub(super) outgoing: Vec<DiskAtomRef>,
	/// Incoming associated with this node adjacency.
	pub(super) incoming: Vec<DiskAtomRef>,
	/// Undirected associated with this node adjacency.
	pub(super) undirected: Vec<DiskAtomRef>,
}

/// Resolves immutable bulk owners through the shared physical topology codec.
pub(super) fn encode_topology(
	topology: &EdgeTopology,
	locals: &BTreeMap<AtomId, DiskAtomRef>,
	arena: &GraphArena<'_>,
) -> FerrosGrynnResult<EncodedTopology> {
	resolve_topology(topology, |endpoint| match endpoint {
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
}

/// Encodes hot side.
pub(super) fn encode_hot_side(
	entries: &[EncodedEndpoint],
	side: EndpointSide,
	cross_level: bool,
	page_lsn: PageLsn,
	local: &mut OverflowBuilder,
	qualified: &mut QualifiedOverflowBuilder,
) -> FerrosGrynnResult<EncodedHotSide> {
	let plan = plan_hot_side(entries, cross_level);
	let mut encoded = EncodedHotSide {
		inline: plan.inline,
		local_overflow: None,
		graph_slot: plan.graph_slot,
		graph_overflow: None,
	};
	match plan.overflow {
		HotSideOverflow::None => {}
		HotSideOverflow::LocalSuffix(entries) => {
			let atoms = entries.iter().map(|entry| entry.atom).collect::<Vec<_>>();
			encoded.local_overflow = local.append(&atoms, page_lsn)?;
		}
		HotSideOverflow::QualifiedWhole(entries) => {
			encoded.graph_overflow = qualified.append(entries, side, page_lsn)?;
		}
	}
	Ok(encoded)
}

/// Records node adjacency.
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

/// Returns the invalid result.
fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

//! Shared resolution of runtime endpoints into checked physical topology.

use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{EdgeEndpoint, EdgeTopology},
	storage::graph_pages::EdgeTopologyCode,
	values::storage::DiskAtomRef,
};

/// One generation-free address retains its exact root-arena scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::storage) struct EncodedEndpoint {
	/// Zero denotes the root graph; nonzero slots preserve qualified identity.
	pub(in crate::storage) graph_slot: u32,
	/// Address already validated by the caller's snapshot-specific resolver.
	pub(in crate::storage) atom: DiskAtomRef,
}

/// Physical addresses of one role after its overflow writer finishes.
///
/// Both bulk and point publishers pass these fields to the same Hot validator.
pub(in crate::storage) struct EncodedHotSide {
	/// First two local addresses, or one qualified inline scope.
	pub(in crate::storage) inline: [Option<DiskAtomRef>; 2],
	/// Ordinary adjacency suffix; qualified sides never use this field.
	pub(in crate::storage) local_overflow: Option<super::PageNo>,
	/// Shared arena slot for an inline qualified side.
	pub(in crate::storage) graph_slot: Option<u32>,
	/// Entire qualified side when inline scope encoding does not fit.
	pub(in crate::storage) graph_overflow: Option<super::PageNo>,
}

/// The physical writer receives only the slice belonging to its overflow codec.
pub(in crate::storage) enum HotSideOverflow<'entries> {
	/// Every endpoint fits in the Hot slot.
	None,
	/// Ordinary overflow stores only the suffix after the two inline entries.
	LocalSuffix(&'entries [EncodedEndpoint]),
	/// Qualified overflow stores the entire role, including any root scope
	/// zero.
	QualifiedWhole(&'entries [EncodedEndpoint]),
}

/// Borrowed layout selection is independent of page allocation and admission.
pub(in crate::storage) struct HotSidePlan<'entries> {
	/// Inline local addresses or a single qualified scope.
	pub(in crate::storage) inline: [Option<DiskAtomRef>; 2],
	/// An inline qualified role keeps its explicit root-arena slot, including
	/// zero.
	pub(in crate::storage) graph_slot: Option<u32>,
	/// At most one overflow representation is selected.
	pub(in crate::storage) overflow: HotSideOverflow<'entries>,
}

/// Preserves format-v1 layout without copying, sorting or deduplicating a role.
///
/// Writers retain their existing allocation, count checks and error order.
pub(in crate::storage) fn plan_hot_side(
	entries: &[EncodedEndpoint],
	cross_level: bool,
) -> HotSidePlan<'_> {
	let qualified_overflow = cross_level
		&& (entries.len() > 2
			|| entries.first().is_some_and(|first| {
				entries.iter().any(|entry| entry.graph_slot != first.graph_slot)
			}));
	if qualified_overflow {
		// Mixing scopes cannot use inline addresses, even when the role has
		// only two entries.
		return HotSidePlan {
			inline: [None; 2],
			graph_slot: None,
			overflow: HotSideOverflow::QualifiedWhole(entries),
		};
	}
	HotSidePlan {
		inline: std::array::from_fn(|index| entries.get(index).map(|entry| entry.atom)),
		graph_slot: cross_level.then(|| entries.first().map(|entry| entry.graph_slot)).flatten(),
		overflow: if !cross_level && entries.len() > 2 {
			HotSideOverflow::LocalSuffix(&entries[2..])
		} else {
			HotSideOverflow::None
		},
	}
}

/// Role order and resolved identity determine the canonical format code.
#[derive(Debug)]
pub(in crate::storage) struct EncodedTopology {
	/// Concrete binary, loop or hyperedge representation.
	pub(in crate::storage) code: EdgeTopologyCode,
	/// Source role, or the complete endpoint set of an undirected edge.
	pub(in crate::storage) source: Vec<EncodedEndpoint>,
	/// Target role remains empty for an undirected edge.
	pub(in crate::storage) target: Vec<EncodedEndpoint>,
}

/// Resolves source before target while preserving each role's endpoint order.
///
/// The caller owns output admission and graph-family topology validation.
///
/// # Errors
/// Propagates endpoint resolution errors and rejects unsupported format-v1
/// bidirectional arity before returning a physical topology.
pub(in crate::storage) fn resolve_topology(
	topology: &EdgeTopology,
	mut resolve: impl FnMut(&EdgeEndpoint) -> FerrosGrynnResult<EncodedEndpoint>,
) -> FerrosGrynnResult<EncodedTopology> {
	match topology {
		EdgeTopology::Directed {
			source,
			target,
		} => {
			let source = resolve_endpoints(source, &mut resolve)?;
			let target = resolve_endpoints(target, &mut resolve)?;
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
			let source = resolve_endpoints(source, &mut resolve)?;
			let target = resolve_endpoints(target, &mut resolve)?;
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
			let source = resolve_endpoints(endpoints, &mut resolve)?;
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

		_ => Err(crate::gql_status::FerrosGrynnError::invalid_argument(
			"Unsupported runtime variant at the workspace boundary.",
		)),
	}
}

/// Retains only resolved addresses; runtime topology and arena remain borrowed.
fn resolve_endpoints(
	endpoints: &[EdgeEndpoint],
	resolve: &mut impl FnMut(&EdgeEndpoint) -> FerrosGrynnResult<EncodedEndpoint>,
) -> FerrosGrynnResult<Vec<EncodedEndpoint>> {
	endpoints.iter().map(resolve).collect()
}
#[cfg(test)]
/// Resolution preserves role order and scoped physical identity.
mod tests;

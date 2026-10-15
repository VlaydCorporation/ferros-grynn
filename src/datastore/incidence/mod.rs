//! Qualified incidence resolves each owner in its actual graph before the
//! terminal.

use super::{DatabaseRoot, GraphOccurrence};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{EdgeEndpoint, EdgeTopology, GraphId},
};

#[cfg(test)]
mod tests;

impl DatabaseRoot {
	/// Visits the ordered owners without collapsing repeated routes to a shared
	/// child, then returns the terminal occurrence in its resolved graph.
	///
	/// # Errors
	///
	/// Returns an absent owner/terminal, non-container owner, or visitor
	/// failure.
	pub(crate) fn resolve_incidence_endpoint(
		&self,
		origin: GraphId,
		endpoint: &EdgeEndpoint,
		mut owner: impl FnMut(GraphId, &GraphOccurrence) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<(GraphId, &GraphOccurrence)> {
		let (scope, terminal) = match endpoint {
			EdgeEndpoint::Local(local) => (&[][..], *local),
			EdgeEndpoint::Nested(qualified) => (qualified.scope(), qualified.atom()),
		};
		if scope.len() > 64 {
			return Err(FerrosGrynnError::invalid_argument(
				"edge endpoint scope exceeds 64 levels",
			));
		}
		let mut graph = origin;
		for local in scope {
			let occurrence = self.occurrence(graph, *local).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("qualified incidence owner is absent")
			})?;
			owner(graph, occurrence)?;
			graph = occurrence.child_graph().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"qualified incidence owner has no child graph",
				)
			})?;
		}
		self.occurrence(graph, terminal).map(|occurrence| (graph, occurrence)).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("qualified incidence terminal is absent")
		})
	}

	/// Validates complete cross-level endpoints before any root mutation.
	///
	/// # Errors
	///
	/// Returns a malformed, absent or over-depth qualified endpoint.
	pub(crate) fn validate_qualified_incidence(
		&self,
		graph: GraphId,
		topology: &EdgeTopology,
	) -> FerrosGrynnResult<()> {
		for endpoint in topology.endpoints().filter(|endpoint| endpoint.is_cross_level()) {
			self.resolve_incidence_endpoint(graph, endpoint, |_graph, _owner| Ok(())).map_err(
				|error| {
					FerrosGrynnError::graph_constraint_violation(
						graph.to_string(),
						"qualified incidence requires an existing complete owner chain and \
						 terminal",
					)
					.with_cause(error)
				},
			)?;
		}
		Ok(())
	}

	/// Owner rebinding, terminal removal and graph retirement can invalidate
	/// connectors in another graph; validate the final candidate before WAL.
	///
	/// # Errors
	///
	/// Returns a dangling or malformed cross-level endpoint in any graph.
	pub(crate) fn validate_all_qualified_incidence(&self) -> FerrosGrynnResult<()> {
		for (graph, root) in &self.graphs {
			for occurrence in root.occurrences() {
				if let super::OccurrenceStateRef::Edge {
					topology,
					..
				} = occurrence.state().view()
				{
					self.validate_qualified_incidence(*graph, topology)?;
				}
			}
		}
		Ok(())
	}

	/// Unchanged connectors keep logical endpoints when an owner or child arena
	/// is replaced. An explicit topology/replacement decision establishes a new
	/// reference instead.
	///
	/// # Errors
	///
	/// Returns a surviving physical handle rebound to another logical identity.
	pub(crate) fn validate_preserved_qualified_incidence(
		&self,
		previous: &Self,
		replaced: impl Fn(GraphId, &GraphOccurrence) -> bool,
	) -> FerrosGrynnResult<()> {
		for (graph, root) in &previous.graphs {
			for old in root.occurrences() {
				if replaced(*graph, old) {
					continue;
				}
				let super::OccurrenceStateRef::Edge {
					topology: old_topology,
					..
				} = old.state().view()
				else {
					continue;
				};
				if !old_topology.is_cross_level() {
					continue;
				}
				let Some(current) = self
					.membership(*graph, old.global_id())
					.and_then(|local| self.occurrence(*graph, local))
				else {
					continue;
				};
				let super::OccurrenceStateRef::Edge {
					topology,
					..
				} = current.state().view()
				else {
					continue;
				};
				if current.local_id() != old.local_id() || topology != old_topology {
					continue;
				}
				for endpoint in topology.endpoints().filter(|endpoint| endpoint.is_cross_level()) {
					let mut old_graph = *graph;
					let changed = || {
						FerrosGrynnError::graph_constraint_violation(
							graph.to_string(),
							"qualified endpoint identity changed; explicitly replace the \
							 connector topology",
						)
					};
					let (new_graph, new_terminal) = self.resolve_incidence_endpoint(
						*graph,
						endpoint,
						|new_graph, new_owner| {
							let old_owner = previous
								.occurrence(old_graph, new_owner.local_id())
								.ok_or_else(changed)?;
							if old_graph != new_graph
								|| old_owner.global_id() != new_owner.global_id()
							{
								return Err(changed());
							}
							old_graph = old_owner.child_graph().ok_or_else(changed)?;
							Ok(())
						},
					)?;
					let old_terminal = previous
						.occurrence(old_graph, new_terminal.local_id())
						.ok_or_else(changed)?;
					if old_graph != new_graph
						|| old_terminal.global_id() != new_terminal.global_id()
					{
						return Err(changed());
					}
				}
			}
		}
		Ok(())
	}
}

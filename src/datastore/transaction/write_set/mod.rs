//! Logical write-set payloads are independent of deduplicated conflict keys.

use std::collections::{BTreeMap, BTreeSet};

mod catalog;
pub(super) use catalog::{
	analyzer_payload_bytes,
	class_payload_bytes,
	field_payload_bytes,
	function_payload_bytes,
	index_payload_bytes,
};

use super::{DatabaseRoot, Mutation, OccurrenceStateRef, Transaction};
use crate::{
	datastore::OverlayValue,
	gql_status::FerrosGrynnResult,
	graph::{AtomId, AtomKind, EdgeEndpoint, EdgeTopology, GraphId, estimated_topology_heap_bytes},
	values::{PropertyKeyId, Text},
};

impl Mutation {
	/// Borrows ordinary operands without materializing effective properties.
	fn payload_heap_bytes(&self) -> usize {
		match self {
			Self::DefineClass(class) | Self::ReplaceClass(class) => class_payload_bytes(class),
			Self::DefineFunction(function) | Self::ReplaceFunction(function) => {
				function_payload_bytes(function)
			}
			Self::DefineAnalyzer(analyzer) | Self::ReplaceAnalyzer(analyzer) => {
				analyzer_payload_bytes(analyzer)
			}
			Self::CreateAtom {
				record_id,
				properties,
				..
			} => record_id.estimated_heap_bytes().saturating_add(properties.estimated_heap_bytes()),
			Self::SetBaseProperty {
				value,
				..
			}
			| Self::SetOverlay {
				value,
				..
			} => value.estimated_heap_bytes(),
			Self::InternProperty {
				name,
				..
			} => name.estimated_heap_bytes(),
			Self::AttachNode {
				label,
				..
			}
			| Self::SetOccurrenceLabel {
				label,
				..
			} => label.as_ref().map_or(0, crate::values::Text::estimated_heap_bytes),
			Self::AttachEdge {
				label,
				topology,
				..
			} => label
				.as_ref()
				.map_or(0, crate::values::Text::estimated_heap_bytes)
				.saturating_add(estimated_topology_heap_bytes(topology)),
			Self::ReplaceOccurrenceState {
				state,
				..
			} => state_payload_bytes(state.view()),
			Self::DefineGraph(graph) | Self::ReplaceGraph(graph) => graph_payload_bytes(graph),
			Self::DefineParameter(parameter) | Self::ReplaceParameter(parameter) => {
				parameter_payload_bytes(parameter)
			}
			_ => 0,
		}
	}
}

/// Graph metadata retains recursive properties and an optional composite root.
pub(super) fn graph_payload_bytes(graph: &crate::datastore::GraphSchema) -> usize {
	graph
		.name()
		.estimated_heap_bytes()
		.saturating_add(graph.properties().estimated_heap_bytes())
		.saturating_add(graph.root().map_or(0, crate::values::RecordId::estimated_heap_bytes))
}

/// Parameters retain their value, catalog name and optional comment.
pub(super) fn parameter_payload_bytes(parameter: &crate::datastore::SchemaParameter) -> usize {
	parameter
		.name()
		.estimated_heap_bytes()
		.saturating_add(parameter.value().estimated_heap_bytes())
		.saturating_add(parameter.comment().map_or(0, crate::values::Text::estimated_heap_bytes))
}

/// Lookup and statistics each retain a separate owned label key.
pub(super) fn label_sidecar_bytes(label: Option<&Text>) -> usize {
	label.map_or(0, Text::estimated_heap_bytes).saturating_mul(2)
}

impl Transaction {
	/// Includes the journal operand and its separately retained private value.
	pub(super) fn mutation_payload_charge(heap_bytes: usize) -> usize {
		size_of::<Mutation>().saturating_add(heap_bytes.saturating_mul(2))
	}

	/// Records only successfully applied operands; failed source writes pay
	/// nothing.
	pub(super) fn record_mutation(&mut self, mutation: Mutation) {
		let sidecars = match &mutation {
			Mutation::AttachNode {
				label,
				..
			}
			| Mutation::AttachEdge {
				label,
				..
			}
			| Mutation::SetOccurrenceLabel {
				label,
				..
			} => label_sidecar_bytes(label.as_ref()),
			_ => 0,
		};
		self.payload_bytes = self.payload_bytes.saturating_add(
			Self::mutation_payload_charge(mutation.payload_heap_bytes()).saturating_add(sidecars),
		);
		self.mutations.push(mutation);
	}

	/// Performs the recursive precommit audit before any publication lock or
	/// I/O.
	pub(super) fn validate_write_set_payload(&mut self) -> FerrosGrynnResult<()> {
		let bytes = self.recursive_write_set_bytes();
		let additional = bytes.saturating_sub(self.write_set_bytes());
		self.ensure_capacity(additional)?;
		self.payload_bytes = self.payload_bytes.saturating_add(additional);
		Ok(())
	}

	/// Scans the retained journal once, including preceding structural
	/// payloads.
	pub(super) fn recursive_write_set_bytes(&self) -> usize {
		self.recursive_write_set_bytes_in(
			&self.snapshot,
			&self.working,
			&super::commit::ReplayRemap::default(),
		)
	}

	/// Rechecks the actual replay image, including concurrent endpoint growth.
	pub(super) fn validate_candidate_payload(
		&self,
		before: &DatabaseRoot,
		after: &DatabaseRoot,
		remap: &super::commit::ReplayRemap,
	) -> FerrosGrynnResult<()> {
		let initial_fanout =
			self.fanout_reservations.len().saturating_mul(super::EFFECTIVE_FANOUT_WRITE_BYTES);
		let bytes = self
			.recursive_write_set_bytes_in(before, after, remap)
			.saturating_sub(initial_fanout)
			.saturating_add(self.commit_fanout_bytes(before));
		if bytes > self.store.inner.transaction_limits().max_write_set_bytes {
			return Err(self.limit_error(bytes));
		}
		Ok(())
	}

	/// Uses final allocation identities for both sides of a replayed delta.
	fn recursive_write_set_bytes_in(
		&self,
		before: &DatabaseRoot,
		after: &DatabaseRoot,
		remap: &super::commit::ReplayRemap,
	) -> usize {
		let admitted = self.write_set_bytes();
		if self.mutations.is_empty() {
			return admitted;
		}
		let mut bytes = admitted
			.saturating_sub(self.payload_bytes)
			.saturating_add(self.mutations.capacity().saturating_mul(size_of::<Mutation>()));
		let mut atoms = BTreeSet::new();
		let mut occurrences = BTreeSet::new();
		let mut structural = BTreeSet::new();
		let mut whole_graphs = BTreeSet::new();
		for mutation in &self.mutations {
			bytes = bytes.saturating_add(mutation.payload_heap_bytes());
			match mutation {
				Mutation::AttachEdge {
					graph,
					local,
					..
				}
				| Mutation::ReplaceOccurrenceState {
					graph,
					local,
					..
				} => {
					structural.insert((*graph, remap.local(*graph, *local)));
				}
				Mutation::RemoveGraph(graph) => {
					whole_graphs.insert(*graph);
				}
				Mutation::InstallBuiltGraphTree(graphs) => {
					whole_graphs.extend(graphs.graph_ids());
				}
				_ => {}
			}
			match mutation {
				Mutation::CreateAtom {
					id,
					..
				} => {
					atoms.insert(remap.global(*id));
				}
				Mutation::SetBaseProperty {
					global,
					..
				}
				| Mutation::RemoveBaseProperty {
					global,
					..
				}
				| Mutation::DeleteAtom {
					global,
				} => {
					atoms.insert(remap.global(*global));
				}
				Mutation::AttachNode {
					graph,
					local,
					global,
					..
				}
				| Mutation::AttachEdge {
					graph,
					local,
					global,
					..
				} => {
					atoms.insert(remap.global(*global));
					occurrences.insert((*graph, remap.local(*graph, *local)));
				}
				Mutation::SetOverlay {
					graph,
					local,
					..
				}
				| Mutation::HideProperty {
					graph,
					local,
					..
				}
				| Mutation::ResetProperty {
					graph,
					local,
					..
				}
				| Mutation::SetOccurrenceLabel {
					graph,
					local,
					..
				}
				| Mutation::ReplaceOccurrenceState {
					graph,
					local,
					..
				} => {
					occurrences.insert((*graph, remap.local(*graph, *local)));
				}
				Mutation::Detach {
					graph,
					global,
				} => {
					atoms.insert(remap.global(*global));
					for root in [before, after] {
						if let Some(local) = root.membership(*graph, remap.global(*global)) {
							occurrences.insert((*graph, local));
							structural.insert((*graph, local));
						}
					}
				}
				_ => {}
			}
		}
		for root in [before, after] {
			if whole_graphs.is_empty() {
				break;
			}
			let graph_ids = root
				.schema
				.all_graphs()
				.filter(|schema| {
					whole_graphs.contains(&schema.id())
						|| schema.owner_root().is_some_and(|owner| whole_graphs.contains(&owner))
				})
				.map(|schema| schema.id());
			for graph in graph_ids {
				if let Some(schema) = root.schema.graph(graph) {
					bytes = bytes.saturating_add(graph_payload_bytes(&schema));
				}
				if let Some(source) = root.graph(graph) {
					for occurrence in source.occurrences() {
						let target = (graph, occurrence.local_id());
						occurrences.insert(target);
						structural.insert(target);
					}
				}
			}
		}
		let mut nodes = BTreeMap::<GraphId, BTreeSet<AtomId>>::new();
		for root in [before, after] {
			for &(graph, local) in &occurrences {
				let Some(occurrence) = root.occurrence(graph, local) else {
					continue;
				};
				atoms.insert(occurrence.global_id());
				bytes = bytes.saturating_add(occurrence_payload_bytes(occurrence));
				if structural.contains(&(graph, local))
					&& let OccurrenceStateRef::Edge {
						topology,
						..
					} = occurrence.state().view()
				{
					for local in topology.endpoints().filter_map(EdgeEndpoint::local_id) {
						if local.kind() == Some(AtomKind::Node) {
							nodes.entry(graph).or_default().insert(local);
						}
					}
				}
			}
		}
		for root in [before, after] {
			for &global in &atoms {
				if let Some(atom) = root.atom(global) {
					bytes = bytes
						.saturating_add(atom.record_id().estimated_heap_bytes())
						.saturating_add(atom.properties().estimated_heap_bytes());
				}
			}
			bytes = bytes.saturating_add(adjacency_bytes(root, &nodes));
		}
		admitted.max(bytes)
	}
}

/// Estimates the heap retained by an occurrence's owned state and sparse
/// overlay.
fn occurrence_payload_bytes(occurrence: &crate::datastore::GraphOccurrence) -> usize {
	let overlay = occurrence.overlay().map_or(0, |overlay| {
		overlay.iter().fold(0_usize, |bytes, (_key, value)| {
			bytes.saturating_add(size_of::<(PropertyKeyId, OverlayValue)>() + 64).saturating_add(
				match value {
					OverlayValue::Value(value) => value.estimated_heap_bytes(),
					OverlayValue::Hidden => 0,
				},
			)
		})
	});
	occurrence
		.label()
		.map_or(0, crate::values::Text::estimated_heap_bytes)
		.saturating_add(label_sidecar_bytes(occurrence.label()))
		.saturating_add(state_payload_bytes(occurrence.state().view()))
		.saturating_add(overlay)
}

/// Qualified endpoint owner chains belong to the topology's recursive payload.
pub(super) fn state_payload_bytes(state: OccurrenceStateRef<'_>) -> usize {
	match state {
		OccurrenceStateRef::Node {
			..
		} => 0,
		OccurrenceStateRef::Edge {
			topology,
			..
		} => estimated_topology_heap_bytes(topology),
	}
}

/// Counts every incident entry of affected nodes, including unchanged edges.
fn adjacency_bytes(root: &DatabaseRoot, touched: &BTreeMap<GraphId, BTreeSet<AtomId>>) -> usize {
	// shortcut: structural admission scans graph incidences; replace this with
	// bounded degree metadata before closing the Q-01 point-DML performance
	// gate.
	let mut bytes = 0_usize;
	for (&graph, nodes) in touched {
		let Some(source) = root.graph(graph) else {
			continue;
		};
		let mut counts = nodes.iter().map(|&node| (node, [0_usize; 3])).collect::<BTreeMap<_, _>>();
		for occurrence in source.occurrences() {
			let OccurrenceStateRef::Edge {
				topology,
				..
			} = occurrence.state().view()
			else {
				continue;
			};
			let mut append = |endpoint: &EdgeEndpoint, sides: &[usize]| {
				if let Some(counts) = endpoint.local_id().and_then(|node| counts.get_mut(&node)) {
					for &side in sides {
						counts[side] = counts[side].saturating_add(1);
					}
				}
			};
			match topology {
				EdgeTopology::Directed {
					source,
					target,
				} => {
					for endpoint in source {
						append(endpoint, &[0]);
					}
					for endpoint in target {
						append(endpoint, &[1]);
					}
				}
				EdgeTopology::Bidirectional {
					source,
					target,
				} => {
					for endpoint in source.iter().chain(target) {
						append(endpoint, &[0, 1]);
					}
				}
				EdgeTopology::Undirected {
					endpoints,
				} => {
					for endpoint in endpoints {
						append(endpoint, &[2]);
					}
				}
			}
		}
		for counts in counts.values() {
			for &count in counts {
				let capacity = if count == 0 {
					0
				} else {
					count.checked_next_power_of_two().unwrap_or(usize::MAX).max(4)
				};
				bytes = bytes
					.saturating_add(size_of::<Vec<AtomId>>())
					.saturating_add(capacity.saturating_mul(size_of::<AtomId>()));
			}
		}
	}
	bytes
}

//! Transactional exact-counter deltas for point mutations.

use super::{LabelStatisticKey, StatisticsStore};
use crate::{
	datastore::{BaseAtom, ClassId, GraphOccurrence, OccurrenceState},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomKind, EdgeTopology, GraphId},
	values::{PropertyKeyId, PropertyMap},
};

impl StatisticsStore {
	pub(crate) fn insert_base(&mut self, class: ClassId) -> FerrosGrynnResult<()> {
		increment(self.base_mut().classes.entry(class).or_default(), 1, "base class count")?;
		self.mark_base_dirty(1)
	}

	pub(crate) fn remove_base(&mut self, class: ClassId) -> FerrosGrynnResult<()> {
		decrement(
			self.base_mut()
				.classes
				.get_mut(&class)
				.ok_or_else(|| invalid("base class statistic is absent"))?,
			1,
			"base class count",
		)?;
		self.mark_base_dirty(1)
	}

	pub(crate) fn replace_base_property(
		&mut self,
		before: &BaseAtom,
		after: &BaseAtom,
		key: PropertyKeyId,
		memberships: &[(GraphId, GraphOccurrence)],
	) -> FerrosGrynnResult<()> {
		self.mark_base_dirty(1)?;
		let before_present = before.properties().contains_key(key);
		let after_present = after.properties().contains_key(key);
		for (graph, occurrence) in memberships {
			if !occurrence.inherits(key) {
				continue;
			}
			self.mark_graph_dirty(*graph, 1)?;
			if before_present != after_present {
				self.adjust_label_property_count(
					*graph,
					occurrence,
					if after_present {
						1
					} else {
						-1
					},
				)?;
			}
		}
		Ok(())
	}

	pub(crate) fn insert_occurrence(
		&mut self,
		graph: GraphId,
		atom: &BaseAtom,
		occurrence: &GraphOccurrence,
	) -> FerrosGrynnResult<()> {
		let property_count =
			u64::try_from(effective_property_count(atom.properties(), occurrence)?)
				.map_err(|_error| invalid("effective property count exceeds u64"))?;
		let topology = match occurrence.state() {
			OccurrenceState::Node {
				..
			} => None,
			OccurrenceState::Edge {
				topology,
				..
			} => Some(topology),
		};
		self.insert_occurrence_parts(
			graph,
			atom.class_id(),
			atom.kind(),
			occurrence.label(),
			topology,
			property_count,
		)
	}

	pub(crate) fn insert_fresh_occurrence(
		&mut self,
		graph: GraphId,
		atom: &BaseAtom,
		label: Option<&crate::values::Text>,
		topology: Option<&EdgeTopology>,
	) -> FerrosGrynnResult<()> {
		let property_count = u64::try_from(atom.properties().len())
			.map_err(|_error| invalid("base property count exceeds u64"))?;
		self.insert_occurrence_parts(
			graph,
			atom.class_id(),
			atom.kind(),
			label,
			topology,
			property_count,
		)
	}

	fn insert_occurrence_parts(
		&mut self,
		graph: GraphId,
		class: ClassId,
		kind: AtomKind,
		label: Option<&crate::values::Text>,
		topology: Option<&EdgeTopology>,
		property_count: u64,
	) -> FerrosGrynnResult<()> {
		let statistics =
			self.graph_mut(graph).ok_or_else(|| invalid("statistics graph is absent"))?;
		increment(statistics.classes.entry(class).or_default(), 1, "graph class count")?;
		match (kind, topology) {
			(AtomKind::Node, None) => increment(&mut statistics.node_count, 1, "graph node count")?,
			(AtomKind::Edge, Some(topology)) => {
				increment(&mut statistics.edge_count, 1, "graph edge count")?;
				let [out, incoming, undirected] = topology_counts(topology)?;
				increment(&mut statistics.total_out_degree, out, "total out degree")?;
				increment(&mut statistics.total_in_degree, incoming, "total in degree")?;
				increment(
					&mut statistics.total_undirected_degree,
					undirected,
					"total undirected degree",
				)?;
				statistics.maximum_out_degree =
					statistics.maximum_out_degree.max(statistics.total_out_degree);
				statistics.maximum_in_degree =
					statistics.maximum_in_degree.max(statistics.total_in_degree);
			}
			_ => return Err(invalid("fresh occurrence kind and topology disagree")),
		}
		if let Some(label) = label {
			let label = statistics
				.labels
				.entry(LabelStatisticKey {
					kind,
					label: label.clone(),
				})
				.or_default();
			increment(&mut label.atom_count, 1, "label atom count")?;
			increment(&mut label.property_count, property_count, "label property count")?;
		}
		self.mark_graph_dirty(graph, 1)
	}

	pub(crate) fn remove_occurrence(
		&mut self,
		graph: GraphId,
		atom: &BaseAtom,
		occurrence: &GraphOccurrence,
	) -> FerrosGrynnResult<()> {
		let property_count =
			u64::try_from(effective_property_count(atom.properties(), occurrence)?)
				.map_err(|_error| invalid("effective property count exceeds u64"))?;
		let statistics =
			self.graph_mut(graph).ok_or_else(|| invalid("statistics graph is absent"))?;
		decrement(
			statistics
				.classes
				.get_mut(&atom.class_id())
				.ok_or_else(|| invalid("graph class statistic is absent"))?,
			1,
			"graph class count",
		)?;
		match occurrence.state() {
			OccurrenceState::Node {
				..
			} => decrement(&mut statistics.node_count, 1, "graph node count")?,
			OccurrenceState::Edge {
				topology,
				..
			} => {
				decrement(&mut statistics.edge_count, 1, "graph edge count")?;
				let [out, incoming, undirected] = topology_counts(topology)?;
				decrement(&mut statistics.total_out_degree, out, "total out degree")?;
				decrement(&mut statistics.total_in_degree, incoming, "total in degree")?;
				decrement(
					&mut statistics.total_undirected_degree,
					undirected,
					"total undirected degree",
				)?;
			}
		}
		if let Some(label) = occurrence.label() {
			let label = statistics
				.labels
				.get_mut(&LabelStatisticKey {
					kind: atom.kind(),
					label: label.clone(),
				})
				.ok_or_else(|| invalid("label statistic is absent"))?;
			decrement(&mut label.atom_count, 1, "label atom count")?;
			decrement(&mut label.property_count, property_count, "label property count")?;
		}
		self.mark_graph_dirty(graph, 1)
	}

	pub(crate) fn replace_occurrence(
		&mut self,
		graph: GraphId,
		atom: &BaseAtom,
		before: &GraphOccurrence,
		after: &GraphOccurrence,
	) -> FerrosGrynnResult<()> {
		let before_count = effective_property_count(atom.properties(), before)?;
		let after_count = effective_property_count(atom.properties(), after)?;
		if before.label() != after.label() {
			self.remove_label(graph, atom, before, before_count)?;
			self.add_label(graph, atom, after, after_count)?;
		} else if after_count != before_count {
			let delta = i64::try_from(after_count)
				.ok()
				.and_then(|after| i64::try_from(before_count).ok().map(|before| after - before))
				.ok_or_else(|| invalid("effective property-count delta exceeds i64"))?;
			self.adjust_label_property_count(graph, after, delta)?;
		}
		self.mark_graph_dirty(graph, 1)
	}

	fn add_label(
		&mut self,
		graph: GraphId,
		atom: &BaseAtom,
		occurrence: &GraphOccurrence,
		property_count: usize,
	) -> FerrosGrynnResult<()> {
		let Some(label) = occurrence.label() else {
			return Ok(());
		};
		let statistic = self
			.graph_mut(graph)
			.ok_or_else(|| invalid("statistics graph is absent"))?
			.labels
			.entry(LabelStatisticKey {
				kind: atom.kind(),
				label: label.clone(),
			})
			.or_default();
		increment(&mut statistic.atom_count, 1, "label atom count")?;
		increment(
			&mut statistic.property_count,
			u64::try_from(property_count)
				.map_err(|_error| invalid("effective property count exceeds u64"))?,
			"label property count",
		)
	}

	fn remove_label(
		&mut self,
		graph: GraphId,
		atom: &BaseAtom,
		occurrence: &GraphOccurrence,
		property_count: usize,
	) -> FerrosGrynnResult<()> {
		let Some(label) = occurrence.label() else {
			return Ok(());
		};
		let statistic = self
			.graph_mut(graph)
			.ok_or_else(|| invalid("statistics graph is absent"))?
			.labels
			.get_mut(&LabelStatisticKey {
				kind: atom.kind(),
				label: label.clone(),
			})
			.ok_or_else(|| invalid("label statistic is absent"))?;
		decrement(&mut statistic.atom_count, 1, "label atom count")?;
		decrement(
			&mut statistic.property_count,
			u64::try_from(property_count)
				.map_err(|_error| invalid("effective property count exceeds u64"))?,
			"label property count",
		)
	}

	fn adjust_label_property_count(
		&mut self,
		graph: GraphId,
		occurrence: &GraphOccurrence,
		delta: i64,
	) -> FerrosGrynnResult<()> {
		let Some(label) = occurrence.label() else {
			return Ok(());
		};
		let statistic = self
			.graph_mut(graph)
			.ok_or_else(|| invalid("statistics graph is absent"))?
			.labels
			.get_mut(&LabelStatisticKey {
				kind: occurrence
					.local_id()
					.kind()
					.ok_or_else(|| invalid("occurrence kind is invalid"))?,
				label: label.clone(),
			})
			.ok_or_else(|| invalid("label statistic is absent"))?;
		if delta >= 0 {
			increment(&mut statistic.property_count, delta.unsigned_abs(), "label property count")
		} else {
			decrement(&mut statistic.property_count, delta.unsigned_abs(), "label property count")
		}
	}

	fn mark_base_dirty(&mut self, rows: u64) -> FerrosGrynnResult<()> {
		increment(&mut self.base_mut().dirty_rows, rows, "base dirty rows")?;
		self.base_touched = true;
		Ok(())
	}

	fn mark_graph_dirty(&mut self, graph: GraphId, rows: u64) -> FerrosGrynnResult<()> {
		let statistics =
			self.graph_mut(graph).ok_or_else(|| invalid("statistics graph is absent"))?;
		increment(&mut statistics.dirty_rows, rows, "graph dirty rows")?;
		self.graphs_touched.insert(graph);
		Ok(())
	}
}

fn effective_property_count(
	base: &PropertyMap,
	occurrence: &GraphOccurrence,
) -> FerrosGrynnResult<usize> {
	let inherited = base
		.iter()
		.filter(|(key, value)| occurrence.effective_value(Some(value), *key).is_some())
		.count();
	let additional = occurrence
		.overlay()
		.into_iter()
		.flat_map(crate::datastore::PropertyOverlay::iter)
		.filter(|(key, value)| {
			!base.contains_key(*key) && matches!(value, crate::datastore::OverlayValue::Value(_))
		})
		.count();
	inherited
		.checked_add(additional)
		.ok_or_else(|| invalid("effective property count overflows usize"))
}

fn topology_counts(topology: &EdgeTopology) -> FerrosGrynnResult<[u64; 3]> {
	let local_nodes = |endpoints: &[crate::graph::EdgeEndpoint]| {
		endpoints
			.iter()
			.filter(|endpoint| {
				endpoint.local_id().is_some_and(|id| id.kind() == Some(AtomKind::Node))
			})
			.count()
	};
	let counts = match topology {
		EdgeTopology::Directed {
			source,
			target,
		} => [local_nodes(source), local_nodes(target), 0],
		EdgeTopology::Bidirectional {
			source,
			target,
		} => {
			let both = local_nodes(source)
				.checked_add(local_nodes(target))
				.ok_or_else(|| invalid("bidirectional degree count overflows usize"))?;
			[both, both, 0]
		}
		EdgeTopology::Undirected {
			endpoints,
		} => [0, 0, local_nodes(endpoints)],
	};
	Ok([
		u64::try_from(counts[0]).map_err(|_error| invalid("out-degree count exceeds u64"))?,
		u64::try_from(counts[1]).map_err(|_error| invalid("in-degree count exceeds u64"))?,
		u64::try_from(counts[2]).map_err(|_error| invalid("undirected count exceeds u64"))?,
	])
}

fn increment(target: &mut u64, amount: u64, name: &str) -> FerrosGrynnResult<()> {
	*target = target
		.checked_add(amount)
		.ok_or_else(|| invalid(format!("statistics {name} overflows u64")))?;
	Ok(())
}

fn decrement(target: &mut u64, amount: u64, name: &str) -> FerrosGrynnResult<()> {
	*target = target
		.checked_sub(amount)
		.ok_or_else(|| invalid(format!("statistics {name} underflows u64")))?;
	Ok(())
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

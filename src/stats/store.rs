use std::collections::{BTreeMap, BTreeSet};

use super::{BaseStatistics, GraphStatistics};
use crate::{datastore::ClassId, graph::GraphId};

/// Statistics owned by one immutable database MVCC root.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StatisticsStore {
	base: BaseStatistics,
	graphs: BTreeMap<GraphId, GraphStatistics>,
	pub(super) base_touched: bool,
	pub(super) graphs_touched: BTreeSet<GraphId>,
}

impl StatisticsStore {
	/// Returns database-global Base statistics.
	pub const fn base(&self) -> &BaseStatistics {
		&self.base
	}

	/// Returns statistics for one graph-effective population.
	pub fn graph(&self, graph: GraphId) -> Option<&GraphStatistics> {
		self.graphs.get(&graph)
	}

	pub(crate) fn base_mut(&mut self) -> &mut BaseStatistics {
		&mut self.base
	}

	pub(crate) fn graph_mut(&mut self, graph: GraphId) -> Option<&mut GraphStatistics> {
		self.graphs.get_mut(&graph)
	}

	pub(crate) fn register_class(&mut self, class: ClassId) {
		self.base.classes.entry(class).or_default();
		for statistics in self.graphs.values_mut() {
			statistics.classes.entry(class).or_default();
		}
	}

	pub(crate) fn register_graph(
		&mut self,
		graph: GraphId,
		classes: impl IntoIterator<Item = ClassId>,
	) {
		let statistics = self.graphs.entry(graph).or_default();
		for class in classes {
			statistics.classes.entry(class).or_default();
		}
	}

	pub(crate) fn replace_graph(&mut self, graph: GraphId, statistics: GraphStatistics) {
		self.graphs.insert(graph, statistics);
	}

	pub(crate) fn publish_revision(&mut self, revision: u64) {
		if self.base_touched {
			self.base.revision = revision;
			self.base_touched = false;
		}
		for graph in std::mem::take(&mut self.graphs_touched) {
			if let Some(statistics) = self.graphs.get_mut(&graph) {
				statistics.revision = revision;
			}
		}
	}
}

use std::collections::{BTreeMap, BTreeSet};

use super::{BaseStatistics, GraphStatistics};
use crate::{datastore::ClassId, graph::GraphId};

/// Statistics owned by one immutable database MVCC root.
#[derive(Debug, Clone, PartialEq)]
pub struct StatisticsStore {
	base: BaseStatistics,
	graphs: BTreeMap<GraphId, GraphStatistics>,
	pub(super) base_touched: bool,
	pub(super) graphs_touched: BTreeSet<GraphId>,
	auxiliary_deferred: bool,
}

impl Default for StatisticsStore {
	fn default() -> Self {
		Self {
			base: BaseStatistics::default(),
			graphs: BTreeMap::new(),
			base_touched: false,
			graphs_touched: BTreeSet::new(),
			auxiliary_deferred: false,
		}
	}
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

	/// Marks independently stored statistics parts as absent from this runtime
	/// projection until an exact physical snapshot supplies them.
	pub(crate) fn mark_auxiliary_deferred(&mut self) {
		self.auxiliary_deferred = true;
	}

	/// Returns whether this runtime projection contains only eager roots.
	pub(crate) const fn auxiliary_deferred(&self) -> bool {
		self.auxiliary_deferred
	}

	/// Marks all independently stored statistics parts as present.
	pub(crate) fn mark_auxiliary_complete(&mut self) {
		self.auxiliary_deferred = false;
	}

	/// Reports whether publication changes any part of this statistics
	/// snapshot.
	pub(crate) fn has_revision_updates(&self) -> bool {
		self.base_touched || !self.graphs_touched.is_empty()
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

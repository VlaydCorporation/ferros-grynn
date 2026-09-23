use crate::graph::GraphId;

/// Physical data-space identity used by database and graph-local persistence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum DataSpaceId {
	/// Database-global AtomStore and catalogs.
	Database,
	/// One graph's occurrences, overlays, and incidence.
	Graph(GraphId),
}

impl DataSpaceId {
	/// Decodes the durable integer representation.
	///
	/// Zero denotes database-global storage. Every other value must be a valid
	/// non-zero graph identity.
	pub(crate) fn from_raw(raw: u64) -> Option<Self> {
		if raw == 0 {
			Some(Self::Database)
		} else {
			GraphId::new(raw).map(Self::Graph)
		}
	}

	/// Returns the durable integer representation.
	pub(crate) const fn raw(self) -> u64 {
		match self {
			Self::Database => 0,
			Self::Graph(graph) => graph.get(),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::DataSpaceId;
	use crate::graph::GraphId;

	#[test]
	fn database_space_is_not_a_fake_graph() {
		let graph = GraphId::new(1).unwrap();

		assert_eq!(DataSpaceId::Database.raw(), 0);
		assert_eq!(DataSpaceId::Graph(graph).raw(), 1);
		assert_eq!(DataSpaceId::from_raw(0), Some(DataSpaceId::Database));
		assert_eq!(DataSpaceId::from_raw(1), Some(DataSpaceId::Graph(graph)));
	}
}

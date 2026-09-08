//! Canonical format-v1 graph-generation filenames.

use crate::{datastore::TableIndexKind, storage::types::DataFileKind};

/// Singleton files produced for every immutable graph generation.
pub(in crate::storage) const GRAPH_FILES: [(DataFileKind, &str); 12] = [
	(DataFileKind::GraphSuperblock, "superblock.fgb"),
	(DataFileKind::NodesHot, "nodes_hot.fgb"),
	(DataFileKind::EdgesHot, "edges_hot.fgb"),
	(DataFileKind::AdjacencyOverflow, "adjacency_overflow.fgb"),
	(DataFileKind::CrossLevelAdjacencyOverflow, "adj_over_cl.fgb"),
	(DataFileKind::Properties, "properties.fgb"),
	(DataFileKind::Subgraphs, "subgraphs.fgb"),
	(DataFileKind::Labels, "labels.fgb"),
	(DataFileKind::Statistics, "stats/graph_stats.fgb"),
	(DataFileKind::LabelStatistics, "stats/label_stats.fgb"),
	(DataFileKind::PropertyStatistics, "stats/prop_stats.fgb"),
	(DataFileKind::Index, "primary.fgidx"),
];

/// One freelist filename and the base file whose free pages it describes.
pub(in crate::storage) const FREELIST_FILES: [(DataFileKind, &str); 6] = [
	(DataFileKind::NodesHot, "freelist_nodes.fgb"),
	(DataFileKind::EdgesHot, "freelist_edges.fgb"),
	(DataFileKind::Properties, "freelist_props.fgb"),
	(DataFileKind::AdjacencyOverflow, "freelist_adj.fgb"),
	(DataFileKind::CrossLevelAdjacencyOverflow, "freelist_adj_cl.fgb"),
	(DataFileKind::Subgraphs, "freelist_subgraphs.fgb"),
];

/// Returns the required singleton filename for one graph-file kind.
pub(in crate::storage) fn graph_filename(kind: DataFileKind) -> Option<&'static str> {
	GRAPH_FILES.iter().find_map(|(candidate, name)| (*candidate == kind).then_some(*name))
}

/// Returns the required freelist filename for one serviced data-file kind.
pub(in crate::storage) fn freelist_filename(kind: DataFileKind) -> Option<&'static str> {
	FREELIST_FILES.iter().find_map(|(candidate, name)| (*candidate == kind).then_some(*name))
}

/// Builds a generation-local filename without embedding a user-controlled name.
pub(in crate::storage) fn table_index_filename(
	kind: TableIndexKind,
	ordinal: usize,
	table_id: u32,
) -> String {
	match kind {
		TableIndexKind::Standard => format!("std_{ordinal:04}.fgidx"),
		TableIndexKind::Unique => format!("uniq_{ordinal:04}.fgidx"),
		TableIndexKind::Count => format!("cnt_{table_id}.fgidx"),
	}
}

/// Checks the exact generation-local filename for one cataloged table index.
pub(in crate::storage) fn table_index_path_is_canonical(
	path: &str,
	kind: TableIndexKind,
	ordinal: usize,
	table_id: u32,
) -> bool {
	let expected = table_index_filename(kind, ordinal, table_id);
	path.rsplit('/').next() == Some(expected.as_str())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn table_index_filenames_preserve_kind_and_canonical_ordinal() {
		assert!(table_index_path_is_canonical(
			"graphs/1/generation/idx/std_0007.fgidx",
			TableIndexKind::Standard,
			7,
			11,
		));
		assert!(table_index_path_is_canonical(
			"graphs/1/generation/idx/uniq_0007.fgidx",
			TableIndexKind::Unique,
			7,
			11,
		));
		assert!(!table_index_path_is_canonical(
			"graphs/1/generation/idx/scalar_0007.fgidx",
			TableIndexKind::Standard,
			7,
			11,
		));
		assert!(!table_index_path_is_canonical(
			"graphs/1/generation/idx/std_7.fgidx",
			TableIndexKind::Standard,
			7,
			11,
		));
	}
}

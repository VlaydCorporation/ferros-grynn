#[allow(clippy::wildcard_imports, reason = "page codecs use the graph-page facade contract")]
use super::*;

/// Minimal planner statistics persisted by the first local slice.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::storage) struct GraphStatistics {
	pub(in crate::storage) node_count: u64,
	pub(in crate::storage) edge_count: u64,
	pub(in crate::storage) average_out_degree: f64,
	pub(in crate::storage) average_in_degree: f64,
	pub(in crate::storage) maximum_out_degree: u64,
	pub(in crate::storage) maximum_in_degree: u64,
}

/// Encodes one format-v1 statistics root without speculative histograms.
pub(in crate::storage) fn encode_graph_statistics(
	statistics: GraphStatistics,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<AlignedPage> {
	if !statistics.average_out_degree.is_finite()
		|| !statistics.average_in_degree.is_finite()
		|| statistics.average_out_degree < 0.0
		|| statistics.average_in_degree < 0.0
	{
		return Err(invalid_page(0, "graph degree averages must be finite and non-negative"));
	}
	let mut page =
		AlignedPage::new(PageType::StatisticsRoot, PageFlags::empty(), page_lsn, PageNo::new(0));
	let bytes = page.as_bytes_mut();
	write_u64(bytes, STATS_NODE_COUNT_OFFSET, statistics.node_count);
	write_u64(bytes, STATS_EDGE_COUNT_OFFSET, statistics.edge_count);
	write_u64(bytes, STATS_AVG_OUT_OFFSET, statistics.average_out_degree.to_bits());
	write_u64(bytes, STATS_AVG_IN_OFFSET, statistics.average_in_degree.to_bits());
	write_u64(bytes, STATS_MAX_OUT_OFFSET, statistics.maximum_out_degree);
	write_u64(bytes, STATS_MAX_IN_OFFSET, statistics.maximum_in_degree);
	write_optional_page(bytes, STATS_DEGREE_HISTOGRAM_OFFSET, None);
	page.seal()?;
	Ok(page)
}

/// Decodes and validates one minimal format-v1 statistics root.
pub(in crate::storage) fn decode_graph_statistics(
	page: &AlignedPage,
) -> FerrosGrynnResult<GraphStatistics> {
	let page_no = PageNo::new(0);
	let header = page.validate(page_no)?;
	if header.page_type() != PageType::StatisticsRoot || !header.flags().is_empty() {
		return Err(invalid_page(0, "invalid StatisticsRoot page header"));
	}
	let bytes = page.as_bytes();
	if read_optional_page(bytes, STATS_DEGREE_HISTOGRAM_OFFSET).is_some() {
		return Err(invalid_page(0, "unsupported degree histogram root"));
	}
	validate_zero(&bytes[STATS_RESERVED_OFFSET..], page_no, "statistics reserved bytes")?;
	let statistics = GraphStatistics {
		node_count: read_u64(bytes, STATS_NODE_COUNT_OFFSET),
		edge_count: read_u64(bytes, STATS_EDGE_COUNT_OFFSET),
		average_out_degree: f64::from_bits(read_u64(bytes, STATS_AVG_OUT_OFFSET)),
		average_in_degree: f64::from_bits(read_u64(bytes, STATS_AVG_IN_OFFSET)),
		maximum_out_degree: read_u64(bytes, STATS_MAX_OUT_OFFSET),
		maximum_in_degree: read_u64(bytes, STATS_MAX_IN_OFFSET),
	};
	if !statistics.average_out_degree.is_finite()
		|| !statistics.average_in_degree.is_finite()
		|| statistics.average_out_degree < 0.0
		|| statistics.average_in_degree < 0.0
	{
		return Err(invalid_page(0, "graph degree averages must be finite and non-negative"));
	}
	Ok(statistics)
}

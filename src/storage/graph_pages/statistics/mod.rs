//! Split format-v1 planner-statistics files.

mod class;
mod codec;
mod degree;
mod label;
mod property;
mod root;

#[cfg(test)]
mod tests;

pub(in crate::storage) use class::{
	decode_class_statistics,
	decode_class_statistics_range,
	encode_class_statistics,
};
pub(in crate::storage) use degree::decode_degree_statistics;
pub(in crate::storage) use label::decode_label_statistics;
pub(in crate::storage) use property::{
	decode_property_statistics,
	decode_property_statistics_range,
	encode_property_statistics,
};
pub(in crate::storage) use root::{
	StatisticsLayout,
	decode_statistics_root,
	validate_statistics_file_lengths,
};

use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	stats::GraphStatistics,
	storage::{
		graph_index::LabelDictionary,
		page::{AlignedPage, PageLsn, PageNo},
	},
};

/// Complete graph statistics image, split by read frequency.
pub(in crate::storage) struct EncodedStatisticsFiles {
	/// Eager root followed by degree and graph-local class pages.
	pub(in crate::storage) graph: Vec<AlignedPage>,
	/// Exact label counters.
	pub(in crate::storage) labels: Vec<AlignedPage>,
	/// Optional property histograms.
	pub(in crate::storage) properties: Vec<AlignedPage>,
}

/// Encodes all three graph-local statistics files.
pub(in crate::storage) fn encode_statistics(
	statistics: &GraphStatistics,
	dictionary: &LabelDictionary,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<EncodedStatisticsFiles> {
	root::validate_summary(statistics)?;
	let mut degrees = degree::encode_degree_statistics(statistics, dictionary, page_lsn)?;
	let class_start = 1_u64
		.checked_add(
			u64::try_from(degrees.len())
				.map_err(|_error| invalid("degree page count exceeds u64"))?,
		)
		.ok_or_else(|| invalid("class statistics range start overflows"))?;
	let mut classes = class::encode_class_statistics(&statistics.classes, class_start, page_lsn)?;
	let labels = label::encode_label_statistics(statistics, dictionary, page_lsn)?;
	let properties =
		property::encode_property_statistics(&statistics.property_histograms, page_lsn)?;
	let layout = StatisticsLayout::from_counts(
		degrees.len(),
		classes.len(),
		labels.len(),
		properties.len(),
	)?;
	let mut graph = Vec::new();
	let graph_page_count = degrees
		.len()
		.checked_add(classes.len())
		.and_then(|count| count.checked_add(1))
		.ok_or_else(|| invalid("graph statistics page count overflows usize"))?;
	graph
		.try_reserve_exact(graph_page_count)
		.map_err(|_error| invalid("graph statistics page allocation failed"))?;
	// Construct the over-aligned root in its final allocation. Moving a
	// populated 16 KiB value was observed to corrupt its first byte on MSVC.
	graph.push(AlignedPage::zeroed());
	root::encode_statistics_root(&mut graph[0], statistics, layout, page_lsn)?;
	graph.append(&mut degrees);
	graph.append(&mut classes);
	Ok(EncodedStatisticsFiles {
		graph,
		labels,
		properties,
	})
}

fn checked_page_no(index: usize, base: u64, family: &str) -> FerrosGrynnResult<PageNo> {
	let index = u64::try_from(index)
		.map_err(|_error| invalid(format!("{family} page index exceeds u64")))?;
	base.checked_add(index)
		.map(PageNo::new)
		.ok_or_else(|| invalid(format!("{family} page number overflows")))
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

//! Split format-v1 planner-statistics files.

mod class;
mod codec;
mod degree;
mod label;
mod property;
mod root;

#[cfg(test)]
/// Regression tests for statistics behavior.
mod tests;

pub(in crate::storage) use class::{
	decode_class_statistics,
	decode_class_statistics_bounded,
	decode_class_statistics_range,
	encode_class_statistics,
	rewrite_class_statistics_page,
};
pub(in crate::storage) use degree::decode_degree_statistics;
pub(in crate::storage) use label::{
	LabelStatisticsChanges,
	decode_label_statistics,
	rewrite_label_statistics_page,
};
pub(in crate::storage) use property::{
	decode_property_statistics,
	decode_property_statistics_range,
	encode_property_statistics,
};
pub(in crate::storage) use root::{StatisticsLayout, decode_statistics_root};

use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	stats::GraphStatistics,
	storage::{
		graph_index::LabelDictionary,
		page::{AlignedPage, PAGE_SIZE, PageLsn, PageNo},
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

/// Selected files whose numeric label references require re-encoding.
pub(in crate::storage) struct EncodedGraphLabelStatistics {
	/// Eager root, degree distributions and class counters.
	pub(in crate::storage) graph: Vec<AlignedPage>,
	/// Exact counters in the accepted label namespace.
	pub(in crate::storage) labels: Vec<AlignedPage>,
}

/// Codec-owned sizing for a selected graph file with retained auxiliaries.
pub(in crate::storage) struct GraphStatisticsLayout {
	/// Root, degree and class page count, including private WAL copies.
	pub(in crate::storage) output_pages: usize,
	/// Output page storage and its FileImage page-number directory.
	pub(in crate::storage) output_bytes: usize,
	/// Temporary encoder allocations above the retained output.
	pub(in crate::storage) scratch_bytes: usize,
	/// Exact ranges preserve the independently retained auxiliary lengths.
	layout: StatisticsLayout,
}

impl GraphStatisticsLayout {
	/// Measures the graph encoder without allocating either auxiliary image.
	pub(in crate::storage) fn measure(
		statistics: &GraphStatistics,
		label_pages: usize,
		property_pages: usize,
	) -> FerrosGrynnResult<Self> {
		root::validate_summary(statistics)?;
		let degrees = statistics.degree_histograms.len();
		let (classes, class_scratch) = class::encoding_size(statistics.classes.len())?;
		let layout = StatisticsLayout::from_counts(degrees, classes, label_pages, property_pages)?;
		let output_pages = usize::try_from(layout.graph_len())
			.map_err(|_error| invalid("graph statistics pages exceed usize"))?;
		let output_bytes = output_pages
			.checked_mul(PAGE_SIZE + size_of::<PageNo>())
			.ok_or_else(|| invalid("graph statistics output bytes overflow usize"))?;
		let packing = degrees
			.checked_add(classes)
			.and_then(|pages| pages.checked_mul(PAGE_SIZE))
			.ok_or_else(|| invalid("statistics packing scratch overflows usize"))?;
		let scratch_bytes = packing
			.max(class_scratch)
			.checked_add(PAGE_SIZE)
			.ok_or_else(|| invalid("statistics encoding scratch overflows usize"))?;
		Ok(Self {
			output_pages,
			output_bytes,
			scratch_bytes,
			layout,
		})
	}
}

/// Codec-owned sizing for selected graph/label output and packing scratch.
pub(in crate::storage) struct GraphLabelStatisticsLayout {
	/// Exact selected output page population, also copied into WAL staging.
	pub(in crate::storage) output_pages: usize,
	/// Checked ranges also preserve the unrelated property-file page count.
	layout: StatisticsLayout,
	/// Output pages, including their page-number directory in FileImage.
	pub(in crate::storage) output_bytes: usize,
	/// Maximum additional temporary allocation while encoding those pages.
	pub(in crate::storage) scratch_bytes: usize,
}

impl GraphLabelStatisticsLayout {
	/// Measures all simultaneously live buffers before either file is encoded.
	pub(in crate::storage) fn measure(
		statistics: &GraphStatistics,
		property_pages: usize,
	) -> FerrosGrynnResult<Self> {
		let (labels, label_scratch) = label::encoding_size(statistics.labels.len())?;
		let graph = GraphStatisticsLayout::measure(statistics, labels, property_pages)?;
		let output_pages = graph
			.output_pages
			.checked_add(labels)
			.ok_or_else(|| invalid("selected statistics page count overflows usize"))?;
		let output_bytes = output_pages
			.checked_mul(PAGE_SIZE + size_of::<PageNo>())
			.ok_or_else(|| invalid("selected statistics output bytes overflow usize"))?;
		let label_scratch = label_scratch
			.checked_add(PAGE_SIZE)
			.ok_or_else(|| invalid("label statistics scratch overflows usize"))?;
		let scratch_bytes = graph.scratch_bytes.max(label_scratch);
		Ok(Self {
			output_pages,
			layout: graph.layout,
			output_bytes,
			scratch_bytes,
		})
	}
}

/// Re-encodes label references while retaining the existing property file.
pub(in crate::storage) fn encode_graph_label_statistics(
	statistics: &GraphStatistics,
	dictionary: &LabelDictionary,
	measured: &GraphLabelStatisticsLayout,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<EncodedGraphLabelStatistics> {
	let (degrees, classes) = encode_graph_components(statistics, dictionary, page_lsn)?;
	let labels = label::encode_label_statistics(statistics, dictionary, page_lsn)?;
	let property_pages = usize::try_from(measured.layout.property_pages)
		.map_err(|_error| invalid("property statistics pages exceed usize"))?;
	let layout =
		StatisticsLayout::from_counts(degrees.len(), classes.len(), labels.len(), property_pages)?;
	if layout != measured.layout {
		return Err(invalid("selected statistics sizing disagrees with encoding"));
	}
	let graph = pack_graph_statistics(statistics, layout, degrees, classes, page_lsn)?;
	Ok(EncodedGraphLabelStatistics {
		graph,
		labels,
	})
}

/// Replaces eager metadata without loading or rebuilding auxiliary pages.
pub(in crate::storage) fn rewrite_statistics_root(
	page: &mut AlignedPage,
	statistics: &GraphStatistics,
	graph_pages: u64,
	label_pages: u64,
	property_pages: u64,
) -> FerrosGrynnResult<StatisticsLayout> {
	let (_, layout) = decode_statistics_root(page, graph_pages, label_pages, property_pages)?;
	root::encode_statistics_root(page, statistics, layout, PageLsn::ZERO)?;
	Ok(layout)
}

/// Encodes all three graph-local statistics files.
pub(in crate::storage) fn encode_statistics(
	statistics: &GraphStatistics,
	dictionary: &LabelDictionary,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<EncodedStatisticsFiles> {
	root::validate_summary(statistics)?;
	let (degrees, classes) = encode_graph_components(statistics, dictionary, page_lsn)?;
	let labels = label::encode_label_statistics(statistics, dictionary, page_lsn)?;
	let properties =
		property::encode_property_statistics(&statistics.property_histograms, page_lsn)?;
	let layout = StatisticsLayout::from_counts(
		degrees.len(),
		classes.len(),
		labels.len(),
		properties.len(),
	)?;
	let graph = pack_graph_statistics(statistics, layout, degrees, classes, page_lsn)?;
	Ok(EncodedStatisticsFiles {
		graph,
		labels,
		properties,
	})
}

/// Encodes only graph metadata, preserving both auxiliary-file bindings.
///
/// # Errors
/// Rejects measured-layout drift and propagates existing component codecs.
pub(in crate::storage) fn encode_graph_statistics(
	statistics: &GraphStatistics,
	dictionary: &LabelDictionary,
	measured: &GraphStatisticsLayout,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<Vec<AlignedPage>> {
	let (degrees, classes) = encode_graph_components(statistics, dictionary, page_lsn)?;
	let layout = StatisticsLayout::from_counts(
		degrees.len(),
		classes.len(),
		usize::try_from(measured.layout.label_pages)
			.map_err(|_error| invalid("label statistics page count exceeds usize"))?,
		usize::try_from(measured.layout.property_pages)
			.map_err(|_error| invalid("property statistics page count exceeds usize"))?,
	)?;
	if layout != measured.layout {
		return Err(invalid("selected graph statistics sizing disagrees with encoding"));
	}
	pack_graph_statistics(statistics, layout, degrees, classes, page_lsn)
}

/// Keeps class offsets and degree serialization shared by every graph encoder.
fn encode_graph_components(
	statistics: &GraphStatistics,
	dictionary: &LabelDictionary,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<(Vec<AlignedPage>, Vec<AlignedPage>)> {
	let degrees = degree::encode_degree_statistics(statistics, dictionary, page_lsn)?;
	let class_start = u64::try_from(degrees.len())
		.ok()
		.and_then(|pages| pages.checked_add(1))
		.ok_or_else(|| invalid("class statistics range start overflows u64"))?;
	let classes = class::encode_class_statistics(&statistics.classes, class_start, page_lsn)?;
	Ok((degrees, classes))
}

/// Packs graph metadata independently of the two auxiliary file encoders.
fn pack_graph_statistics(
	statistics: &GraphStatistics,
	layout: StatisticsLayout,
	mut degrees: Vec<AlignedPage>,
	mut classes: Vec<AlignedPage>,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<Vec<AlignedPage>> {
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
	Ok(graph)
}

/// Returns the checked page no result.
fn checked_page_no(index: usize, base: u64, family: &str) -> FerrosGrynnResult<PageNo> {
	let index = u64::try_from(index)
		.map_err(|_error| invalid(format!("{family} page index exceeds u64")))?;
	base.checked_add(index)
		.map(PageNo::new)
		.ok_or_else(|| invalid(format!("{family} page number overflows")))
}

/// Returns the invalid result.
fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

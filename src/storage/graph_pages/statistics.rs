use std::collections::BTreeMap;

#[allow(clippy::wildcard_imports, reason = "page codecs use the graph-page facade contract")]
use super::*;
use crate::{stats::HISTOGRAM_BUCKETS, values::graph::AtomKind};

/// Current payload layout version of the statistics root.
const STATISTICS_LAYOUT_VERSION: u32 = 1;

/// Page ranges declared by `stats/graph_stats.fgb` for all three statistics
/// files.
#[derive(Clone, Copy, Default)]
pub(in crate::storage) struct StatisticsLayout {
	/// First degree-histogram page in `graph_stats.fgb`.
	degree_first: Option<PageNo>,
	/// First page in `label_stats.fgb`.
	label_first: Option<PageNo>,
	/// First page in `prop_stats.fgb`.
	property_first: Option<PageNo>,
	/// Number of degree-histogram pages.
	degree_pages: u32,
	/// Number of label-statistics pages.
	label_pages: u32,
	/// Number of property-histogram pages.
	property_pages: u32,
}

impl StatisticsLayout {
	/// Returns the declared `graph_stats.fgb` length.
	pub(super) fn graph_pages(self) -> FerrosGrynnResult<u64> {
		1_u64.checked_add(u64::from(self.degree_pages)).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("graph statistics page count overflows")
		})
	}

	/// Returns the declared `label_stats.fgb` length.
	pub(super) const fn label_pages(self) -> u64 {
		self.label_pages as u64
	}

	/// Returns the declared `prop_stats.fgb` length.
	pub(super) const fn property_pages(self) -> u64 {
		self.property_pages as u64
	}
}

/// Complete encoded statistics image split by physical responsibility.
pub(in crate::storage) struct EncodedStatisticsFiles {
	/// `stats/graph_stats.fgb` root and degree histograms.
	pub(in crate::storage) graph: Vec<AlignedPage>,
	/// `stats/label_stats.fgb` exact per-label counters.
	pub(in crate::storage) labels: Vec<AlignedPage>,
	/// `stats/prop_stats.fgb` property histograms.
	pub(in crate::storage) properties: Vec<AlignedPage>,
}

/// Encodes a root page without auxiliary distributions.
#[cfg(test)]
pub(in crate::storage) fn encode_graph_statistics(
	statistics: &GraphStatistics,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<Box<AlignedPage>> {
	let mut page = Box::new(AlignedPage::zeroed());
	encode_statistics_root_page(&mut page, statistics, StatisticsLayout::default(), page_lsn)?;
	Ok(page)
}

/// Encodes the complete three-file statistics image.
pub(in crate::storage) fn encode_statistics(
	statistics: &GraphStatistics,
	node_labels: &[String],
	edge_labels: &[String],
	page_lsn: PageLsn,
) -> FerrosGrynnResult<EncodedStatisticsFiles> {
	validate_statistics(statistics)?;
	let mut degree_pages = Vec::with_capacity(statistics.degree_histograms.len());
	for (index, (key, histogram)) in statistics.degree_histograms.iter().enumerate() {
		let page_no = u64::try_from(index)
			.ok()
			.and_then(|index| index.checked_add(1))
			.map(PageNo::new)
			.ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("degree statistics page number overflows")
			})?;
		degree_pages.push(encode_degree_histogram(
			page_no,
			page_lsn,
			key,
			histogram,
			node_labels,
			edge_labels,
		)?);
	}
	let property_pages = encode_property_statistics_pages(statistics, page_lsn)?;
	let label_pages =
		encode_label_statistics_pages(statistics, node_labels, edge_labels, page_lsn)?;
	let degree_page_count = checked_page_count(degree_pages.len())?;
	let label_page_count = checked_page_count(label_pages.len())?;
	let property_page_count = checked_page_count(property_pages.len())?;
	let layout = StatisticsLayout {
		degree_first: (degree_page_count != 0).then(|| PageNo::new(1)),
		label_first: (label_page_count != 0).then(|| PageNo::new(0)),
		property_first: (property_page_count != 0).then(|| PageNo::new(0)),
		degree_pages: degree_page_count,
		label_pages: label_page_count,
		property_pages: property_page_count,
	};
	let mut graph_pages = Vec::with_capacity(degree_pages.len() + 1);
	// Initialize the over-aligned root in its final slot. Moving a populated
	// 16 KiB page here corrupted its first byte on Windows MSVC.
	graph_pages.push(AlignedPage::zeroed());
	encode_statistics_root_page(&mut graph_pages[0], statistics, layout, page_lsn)?;
	graph_pages.append(&mut degree_pages);
	Ok(EncodedStatisticsFiles {
		graph: graph_pages,
		labels: label_pages,
		properties: property_pages,
	})
}

/// Encodes the root and complete label file changed by point DML.
///
/// The over-aligned root is initialized in its final allocation because moving
/// a populated 16 KiB value corrupts its first byte on Windows MSVC.
pub(in crate::storage) fn encode_statistics_delta(
	statistics: &GraphStatistics,
	node_labels: &[String],
	edge_labels: &[String],
	previous: StatisticsLayout,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<(Box<AlignedPage>, Vec<AlignedPage>)> {
	validate_statistics(statistics)?;
	let labels = encode_label_statistics_pages(statistics, node_labels, edge_labels, page_lsn)?;
	let label_pages = checked_page_count(labels.len())?;
	let layout = StatisticsLayout {
		label_first: (label_pages != 0).then(|| PageNo::new(0)),
		label_pages,
		..previous
	};
	validate_layout_starts(layout)?;
	let mut root = Box::new(AlignedPage::zeroed());
	encode_statistics_root_page(&mut root, statistics, layout, page_lsn)?;
	Ok((root, labels))
}

/// Encodes the property-histogram file from zero-based pages.
fn encode_property_statistics_pages(
	statistics: &GraphStatistics,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<Vec<AlignedPage>> {
	let mut pages = Vec::new();
	for (key, histogram) in &statistics.property_histograms {
		let page_no = PageNo::new(u64::try_from(pages.len()).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("property statistics page number exceeds u64")
		})?);
		if let Some(page) = encode_property_histogram(page_no, page_lsn, *key, histogram)? {
			pages.push(page);
		}
	}
	Ok(pages)
}

/// Encodes the exact label-counter file from zero-based pages.
fn encode_label_statistics_pages(
	statistics: &GraphStatistics,
	node_labels: &[String],
	edge_labels: &[String],
	page_lsn: PageLsn,
) -> FerrosGrynnResult<Vec<AlignedPage>> {
	let label_entries = encoded_label_entries(statistics, node_labels, edge_labels)?;
	let label_page_count = label_entries.len().div_ceil(LABEL_STATS_PER_PAGE);
	let mut pages = Vec::with_capacity(label_page_count);
	for (index, entries) in label_entries.chunks(LABEL_STATS_PER_PAGE).enumerate() {
		let index_u64 = u64::try_from(index).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("label statistics page index exceeds u64")
		})?;
		let page_no = PageNo::new(index_u64);
		let next = if index + 1 < label_page_count {
			Some(PageNo::new(page_no.get().checked_add(1).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"label statistics next-page number overflows",
				)
			})?))
		} else {
			None
		};
		pages.push(encode_label_statistics(page_no, page_lsn, next, entries)?);
	}
	Ok(pages)
}

/// Decodes only the root counters, preserving compatibility with old one-page
/// files.
#[cfg(test)]
pub(in crate::storage) fn decode_graph_statistics(
	page: &AlignedPage,
) -> FerrosGrynnResult<GraphStatistics> {
	decode_statistics_root_page(page).map(|(statistics, _layout)| statistics)
}

/// Decodes the eager graph root and validates declared physical lengths.
pub(in crate::storage) fn decode_statistics_summary(
	page: &AlignedPage,
	graph_pages: u64,
	label_pages: u64,
	property_pages: u64,
) -> FerrosGrynnResult<(GraphStatistics, StatisticsLayout)> {
	let (statistics, layout) = decode_statistics_root_page(page)?;
	validate_layout(layout, graph_pages, label_pages, property_pages)?;
	Ok((statistics, layout))
}

/// Returns the split-file directory declared by a statistics root.
pub(in crate::storage) fn statistics_layout(
	page: &AlignedPage,
) -> FerrosGrynnResult<StatisticsLayout> {
	decode_statistics_root_page(page).map(|(_statistics, layout)| layout)
}

/// Returns whether a point delta preserved both bulk histogram ranges.
pub(in crate::storage) fn statistics_bulk_layout_matches(
	source: &AlignedPage,
	target: &AlignedPage,
) -> FerrosGrynnResult<bool> {
	let (_source_statistics, source) = decode_statistics_root_page(source)?;
	let (_target_statistics, target) = decode_statistics_root_page(target)?;
	Ok(source.degree_first == target.degree_first
		&& source.degree_pages == target.degree_pages
		&& source.property_first == target.property_first
		&& source.property_pages == target.property_pages)
}

/// Decodes degree histograms lazily from `graph_stats.fgb`.
pub(in crate::storage) fn decode_degree_statistics<ReadPage>(
	mut read_page: ReadPage,
	layout: StatisticsLayout,
	node_labels: &[String],
	edge_labels: &[String],
) -> FerrosGrynnResult<BTreeMap<DegreeHistogramKey, DegreeHistogram>>
where
	ReadPage: FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
{
	let mut page = AlignedPage::zeroed();
	let mut histograms = BTreeMap::new();
	for offset in 0..u64::from(layout.degree_pages) {
		let page_no = page_at(layout.degree_first, offset, "degree histogram")?;
		read_page(page_no, &mut page)?;
		let (key, histogram) = decode_degree_histogram(&page, page_no, node_labels, edge_labels)?;
		if histograms.insert(key, histogram).is_some() {
			return Err(invalid_page(page_no.get(), "duplicate degree histogram"));
		}
	}
	Ok(histograms)
}

/// Decodes property histograms lazily from `prop_stats.fgb`.
pub(in crate::storage) fn decode_property_statistics<ReadPage>(
	mut read_page: ReadPage,
	layout: StatisticsLayout,
) -> FerrosGrynnResult<BTreeMap<PropertyHistogramKey, PropertyHistogram>>
where
	ReadPage: FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
{
	let mut page = AlignedPage::zeroed();
	let mut histograms = BTreeMap::new();
	for offset in 0..u64::from(layout.property_pages) {
		let page_no = page_at(layout.property_first, offset, "property histogram")?;
		read_page(page_no, &mut page)?;
		let (key, histogram) = decode_property_histogram(&page, page_no)?;
		if histograms.insert(key, histogram).is_some() {
			return Err(invalid_page(page_no.get(), "duplicate property histogram"));
		}
	}
	Ok(histograms)
}

/// Decodes exact label counters lazily from `label_stats.fgb`.
pub(in crate::storage) fn decode_label_statistics_file<ReadPage>(
	mut read_page: ReadPage,
	layout: StatisticsLayout,
	node_labels: &[String],
	edge_labels: &[String],
) -> FerrosGrynnResult<BTreeMap<LabelStatisticKey, LabelStatistic>>
where
	ReadPage: FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
{
	let mut page = AlignedPage::zeroed();
	let mut labels = BTreeMap::new();
	for offset in 0..u64::from(layout.label_pages) {
		let page_no = page_at(layout.label_first, offset, "label statistics")?;
		read_page(page_no, &mut page)?;
		let expected_next =
			(offset + 1 < u64::from(layout.label_pages)).then(|| PageNo::new(page_no.get() + 1));
		for (key, value) in
			decode_label_statistics(&page, page_no, expected_next, node_labels, edge_labels)?
		{
			if labels.insert(key, value).is_some() {
				return Err(invalid_page(page_no.get(), "duplicate label statistics"));
			}
		}
	}
	Ok(labels)
}

/// Writes exact counters and the auxiliary directory into page zero.
fn encode_statistics_root_page(
	page: &mut AlignedPage,
	statistics: &GraphStatistics,
	layout: StatisticsLayout,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<()> {
	validate_statistics(statistics)?;
	page.reset(PageType::StatisticsRoot, PageFlags::empty(), page_lsn, PageNo::new(0));
	let bytes = page.as_bytes_mut();
	write_u64(bytes, STATS_NODE_COUNT_OFFSET, statistics.node_count);
	write_u64(bytes, STATS_EDGE_COUNT_OFFSET, statistics.edge_count);
	write_f64(bytes, STATS_AVG_OUT_OFFSET, statistics.average_out_degree());
	write_f64(bytes, STATS_AVG_IN_OFFSET, statistics.average_in_degree());
	write_u64(bytes, STATS_MAX_OUT_OFFSET, statistics.maximum_out_degree);
	write_u64(bytes, STATS_MAX_IN_OFFSET, statistics.maximum_in_degree);
	write_optional_page(bytes, STATS_DEGREE_HISTOGRAM_OFFSET, layout.degree_first);
	write_optional_page(bytes, STATS_LABEL_FIRST_OFFSET, layout.label_first);
	write_optional_page(bytes, STATS_PROPERTY_FIRST_OFFSET, layout.property_first);
	write_u32(bytes, STATS_LABEL_PAGE_COUNT_OFFSET, layout.label_pages);
	write_u32(bytes, STATS_DEGREE_PAGE_COUNT_OFFSET, layout.degree_pages);
	write_u32(bytes, STATS_PROPERTY_PAGE_COUNT_OFFSET, layout.property_pages);
	write_u32(bytes, STATS_LAYOUT_VERSION_OFFSET, STATISTICS_LAYOUT_VERSION);
	write_u64(bytes, STATS_ANALYZED_REVISION_OFFSET, statistics.analyzed_revision);
	write_u64(bytes, STATS_DIRTY_ROWS_OFFSET, statistics.dirty_rows);
	write_u64(bytes, STATS_TOTAL_OUT_OFFSET, statistics.total_out_degree);
	write_u64(bytes, STATS_TOTAL_IN_OFFSET, statistics.total_in_degree);
	write_u64(bytes, STATS_TOTAL_UNDIRECTED_OFFSET, statistics.total_undirected_degree);
	write_u64(bytes, STATS_REVISION_OFFSET, statistics.revision);
	page.seal()?;
	Ok(())
}

/// Decodes page zero and validates its scalar invariants.
fn decode_statistics_root_page(
	page: &AlignedPage,
) -> FerrosGrynnResult<(GraphStatistics, StatisticsLayout)> {
	let page_no = PageNo::new(0);
	let header = page.validate(page_no)?;
	if header.page_type() != PageType::StatisticsRoot || !header.flags().is_empty() {
		return Err(invalid_page(0, "invalid StatisticsRoot page header"));
	}
	let bytes = page.as_bytes();
	let node_count = read_u64(bytes, STATS_NODE_COUNT_OFFSET);
	let edge_count = read_u64(bytes, STATS_EDGE_COUNT_OFFSET);
	let average_out_degree = read_f64(bytes, STATS_AVG_OUT_OFFSET);
	let average_in_degree = read_f64(bytes, STATS_AVG_IN_OFFSET);
	let maximum_out_degree = read_u64(bytes, STATS_MAX_OUT_OFFSET);
	let maximum_in_degree = read_u64(bytes, STATS_MAX_IN_OFFSET);
	if !average_out_degree.is_finite()
		|| !average_in_degree.is_finite()
		|| average_out_degree < 0.0
		|| average_in_degree < 0.0
	{
		return Err(invalid_page(0, "graph degree averages must be finite and non-negative"));
	}
	let layout_version = read_u32(bytes, STATS_LAYOUT_VERSION_OFFSET);
	if layout_version == 0 {
		validate_zero(
			&bytes[STATS_LABEL_FIRST_OFFSET..],
			page_no,
			"legacy statistics reserved bytes",
		)?;
		if read_optional_page(bytes, STATS_DEGREE_HISTOGRAM_OFFSET).is_some() {
			return Err(invalid_page(0, "legacy statistics root references an auxiliary page"));
		}
		let statistics = GraphStatistics {
			node_count,
			edge_count,
			total_out_degree: legacy_total(average_out_degree, node_count, "out-degree")?,
			total_in_degree: legacy_total(average_in_degree, node_count, "in-degree")?,
			maximum_out_degree,
			maximum_in_degree,
			..GraphStatistics::default()
		};
		return Ok((statistics, StatisticsLayout::default()));
	}
	if layout_version != STATISTICS_LAYOUT_VERSION {
		return Err(invalid_page(0, "unsupported statistics root layout version"));
	}
	validate_zero(&bytes[STATS_RESERVED_OFFSET..], page_no, "statistics reserved bytes")?;
	let statistics = GraphStatistics {
		revision: read_u64(bytes, STATS_REVISION_OFFSET),
		analyzed_revision: read_u64(bytes, STATS_ANALYZED_REVISION_OFFSET),
		dirty_rows: read_u64(bytes, STATS_DIRTY_ROWS_OFFSET),
		node_count,
		edge_count,
		total_out_degree: read_u64(bytes, STATS_TOTAL_OUT_OFFSET),
		total_in_degree: read_u64(bytes, STATS_TOTAL_IN_OFFSET),
		total_undirected_degree: read_u64(bytes, STATS_TOTAL_UNDIRECTED_OFFSET),
		maximum_out_degree,
		maximum_in_degree,
		..GraphStatistics::default()
	};
	validate_statistics(&statistics)?;
	if !approximately_equal(statistics.average_out_degree(), average_out_degree)
		|| !approximately_equal(statistics.average_in_degree(), average_in_degree)
	{
		return Err(invalid_page(0, "statistics averages disagree with exact degree totals"));
	}
	let layout = StatisticsLayout {
		degree_first: read_optional_page(bytes, STATS_DEGREE_HISTOGRAM_OFFSET),
		label_first: read_optional_page(bytes, STATS_LABEL_FIRST_OFFSET),
		property_first: read_optional_page(bytes, STATS_PROPERTY_FIRST_OFFSET),
		label_pages: read_u32(bytes, STATS_LABEL_PAGE_COUNT_OFFSET),
		degree_pages: read_u32(bytes, STATS_DEGREE_PAGE_COUNT_OFFSET),
		property_pages: read_u32(bytes, STATS_PROPERTY_PAGE_COUNT_OFFSET),
	};
	Ok((statistics, layout))
}

/// Validates cross-field invariants of logical graph statistics.
fn validate_statistics(statistics: &GraphStatistics) -> FerrosGrynnResult<()> {
	if statistics.analyzed_revision > statistics.revision {
		return Err(FerrosGrynnError::invalid_storage_file(
			"statistics analysis revision is newer than its graph revision",
		));
	}
	if statistics.node_count == 0
		&& (statistics.total_out_degree != 0
			|| statistics.total_in_degree != 0
			|| statistics.total_undirected_degree != 0
			|| statistics.maximum_out_degree != 0
			|| statistics.maximum_in_degree != 0)
	{
		return Err(FerrosGrynnError::invalid_storage_file(
			"empty graph contains non-zero degree statistics",
		));
	}
	if statistics.maximum_out_degree > statistics.total_out_degree
		|| statistics.maximum_in_degree > statistics.total_in_degree
	{
		return Err(FerrosGrynnError::invalid_storage_file(
			"maximum degree exceeds its exact total",
		));
	}
	if statistics.labels.values().any(|label| label.atom_count == 0 && label.property_count != 0) {
		return Err(FerrosGrynnError::invalid_storage_file(
			"empty label statistic contains properties",
		));
	}
	Ok(())
}

/// Requires each declared range to cover its physical file exactly.
pub(in crate::storage) fn validate_statistics_file_lengths(
	layout: StatisticsLayout,
	graph_pages: u64,
	label_pages: u64,
	property_pages: u64,
) -> FerrosGrynnResult<()> {
	validate_layout(layout, graph_pages, label_pages, property_pages)
}

/// Requires each declared range to cover its physical file exactly.
fn validate_layout(
	layout: StatisticsLayout,
	graph_pages: u64,
	label_pages: u64,
	property_pages: u64,
) -> FerrosGrynnResult<()> {
	validate_layout_starts(layout)?;
	if layout.graph_pages()? != graph_pages
		|| layout.label_pages() != label_pages
		|| layout.property_pages() != property_pages
	{
		return Err(FerrosGrynnError::invalid_storage_file(
			"statistics directory disagrees with its physical file lengths",
		));
	}
	Ok(())
}

/// Validates the fixed zero-based location of every split-file range.
fn validate_layout_starts(layout: StatisticsLayout) -> FerrosGrynnResult<()> {
	required_start(layout.degree_first, layout.degree_pages, 1, "degree")?;
	required_start(layout.label_first, layout.label_pages, 0, "label")?;
	required_start(layout.property_first, layout.property_pages, 0, "property")?;
	Ok(())
}

/// Validates one optional range start against its expected page number.
fn required_start(
	first: Option<PageNo>,
	count: u32,
	expected: u64,
	name: &'static str,
) -> FerrosGrynnResult<u64> {
	match (first, count) {
		(None, 0) => Ok(expected),
		(Some(first), count) if count != 0 && first.get() == expected => Ok(expected),
		_ => Err(FerrosGrynnError::invalid_storage_file(format!(
			"statistics {name} range disagrees with its directory"
		))),
	}
}

/// Resolves one page inside an auxiliary range.
fn page_at(first: Option<PageNo>, offset: u64, name: &'static str) -> FerrosGrynnResult<PageNo> {
	first.and_then(|first| first.get().checked_add(offset).map(PageNo::new)).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file(format!("statistics {name} page range overflows"))
	})
}

/// Resolves label text to sorted combined on-disk label identities.
fn encoded_label_entries<'a>(
	statistics: &'a GraphStatistics,
	node_labels: &[String],
	edge_labels: &[String],
) -> FerrosGrynnResult<Vec<(u32, &'a LabelStatistic)>> {
	let mut entries = statistics
		.labels
		.iter()
		.map(|(key, value)| encoded_label_id(key, node_labels, edge_labels).map(|id| (id, value)))
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	entries.sort_unstable_by_key(|(id, _value)| *id);
	Ok(entries)
}

/// Encodes distinct node/edge label namespaces into one `u32` range.
fn encoded_label_id(
	key: &LabelStatisticKey,
	node_labels: &[String],
	edge_labels: &[String],
) -> FerrosGrynnResult<u32> {
	let (labels, offset) = match key.kind {
		AtomKind::Node => (node_labels, 0_usize),
		AtomKind::Edge => (edge_labels, node_labels.len()),
	};
	let index = labels.iter().position(|label| label == key.label.as_str()).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("statistics label is not interned")
	})?;
	let combined = offset.checked_add(index).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("combined statistics label id overflows")
	})?;
	let id = u32::try_from(combined).map_err(|_error| {
		FerrosGrynnError::invalid_storage_file("combined statistics label id exceeds u32")
	})?;
	if id == NULL_U32 {
		return Err(FerrosGrynnError::invalid_storage_file(
			"combined statistics label id uses the global sentinel",
		));
	}
	Ok(id)
}

/// Resolves a combined on-disk label identity through both dictionaries.
fn decoded_label_id(
	id: u32,
	node_labels: &[String],
	edge_labels: &[String],
) -> FerrosGrynnResult<LabelStatisticKey> {
	let index = usize::try_from(id)
		.map_err(|_error| FerrosGrynnError::invalid_storage_file("label id exceeds usize"))?;
	if let Some(label) = node_labels.get(index) {
		return Ok(LabelStatisticKey {
			kind: AtomKind::Node,
			label: label.clone().into(),
		});
	}
	let edge_index = index.checked_sub(node_labels.len()).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("combined label id underflows edge namespace")
	})?;
	let label = edge_labels.get(edge_index).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("statistics label id is out of range")
	})?;
	Ok(LabelStatisticKey {
		kind: AtomKind::Edge,
		label: label.clone().into(),
	})
}

/// Encodes one page of exact per-label counters.
fn encode_label_statistics(
	page_no: PageNo,
	page_lsn: PageLsn,
	next: Option<PageNo>,
	entries: &[(u32, &LabelStatistic)],
) -> FerrosGrynnResult<AlignedPage> {
	let count = u32::try_from(entries.len()).map_err(|_error| {
		FerrosGrynnError::invalid_storage_file("label statistics page count exceeds u32")
	})?;
	let mut page =
		AlignedPage::new(PageType::LabelStatistic, PageFlags::empty(), page_lsn, page_no);
	let bytes = page.as_bytes_mut();
	write_optional_page(bytes, LABEL_STATS_NEXT_OFFSET, next);
	write_u32(bytes, LABEL_STATS_COUNT_OFFSET, count);
	for (index, (label_id, statistic)) in entries.iter().enumerate() {
		let offset = LABEL_STATS_ENTRIES_OFFSET + index * LABEL_STATS_ENTRY_BYTES;
		write_u32(bytes, offset, *label_id);
		write_u64(bytes, offset + size_of::<u32>(), statistic.atom_count);
		write_u64(bytes, offset + size_of::<u32>() + size_of::<u64>(), statistic.property_count);
	}
	page.seal()?;
	Ok(page)
}

/// Decodes one label page and validates its chain position.
fn decode_label_statistics(
	page: &AlignedPage,
	page_no: PageNo,
	expected_next: Option<PageNo>,
	node_labels: &[String],
	edge_labels: &[String],
) -> FerrosGrynnResult<Vec<(LabelStatisticKey, LabelStatistic)>> {
	let header = page.validate(page_no)?;
	if header.page_type() != PageType::LabelStatistic || !header.flags().is_empty() {
		return Err(invalid_page(page_no.get(), "invalid LabelStatistic page header"));
	}
	let bytes = page.as_bytes();
	if read_optional_page(bytes, LABEL_STATS_NEXT_OFFSET) != expected_next {
		return Err(invalid_page(page_no.get(), "label statistics next-page link is invalid"));
	}
	validate_zero(
		&bytes[LABEL_STATS_PADDING_OFFSET..LABEL_STATS_ENTRIES_OFFSET],
		page_no,
		"label statistics padding",
	)?;
	let count = usize::try_from(read_u32(bytes, LABEL_STATS_COUNT_OFFSET))
		.map_err(|_error| invalid_page(page_no.get(), "label statistics count exceeds usize"))?;
	if count > LABEL_STATS_PER_PAGE {
		return Err(invalid_page(page_no.get(), "label statistics count exceeds page capacity"));
	}
	let mut entries = Vec::with_capacity(count);
	for index in 0..count {
		let offset = LABEL_STATS_ENTRIES_OFFSET + index * LABEL_STATS_ENTRY_BYTES;
		let key = decoded_label_id(read_u32(bytes, offset), node_labels, edge_labels)?;
		let atom_count = read_u64(bytes, offset + size_of::<u32>());
		let property_count = read_u64(bytes, offset + size_of::<u32>() + size_of::<u64>());
		if atom_count == 0 && property_count != 0 {
			return Err(invalid_page(page_no.get(), "invalid label statistics entry"));
		}
		entries.push((key, LabelStatistic {
			atom_count,
			property_count,
		}));
	}
	let used = LABEL_STATS_ENTRIES_OFFSET + count * LABEL_STATS_ENTRY_BYTES;
	validate_zero(&bytes[used..], page_no, "label statistics trailing bytes")?;
	Ok(entries)
}

/// Encodes one bounded degree distribution.
fn encode_degree_histogram(
	page_no: PageNo,
	page_lsn: PageLsn,
	key: &DegreeHistogramKey,
	histogram: &DegreeHistogram,
	node_labels: &[String],
	edge_labels: &[String],
) -> FerrosGrynnResult<AlignedPage> {
	if histogram.buckets.len() > HISTOGRAM_BUCKETS {
		return Err(FerrosGrynnError::invalid_storage_file(
			"degree histogram exceeds the bucket limit",
		));
	}
	let label_id = key
		.label
		.as_ref()
		.map(|label| encoded_label_id(label, node_labels, edge_labels))
		.transpose()?
		.unwrap_or(NULL_U32);
	let count = u16::try_from(histogram.buckets.len()).map_err(|_error| {
		FerrosGrynnError::invalid_storage_file("degree histogram bucket count exceeds u16")
	})?;
	let mut page =
		AlignedPage::new(PageType::DegreeHistogram, PageFlags::empty(), page_lsn, page_no);
	let bytes = page.as_bytes_mut();
	write_u32(bytes, DEGREE_STATS_LABEL_OFFSET, label_id);
	bytes[DEGREE_STATS_DIRECTION_OFFSET] = key.direction as u8;
	write_u16(bytes, DEGREE_STATS_BUCKET_COUNT_OFFSET, count);
	for (index, bucket) in histogram.buckets.iter().enumerate() {
		let offset = DEGREE_STATS_BUCKETS_OFFSET + index * DEGREE_STATS_BUCKET_BYTES;
		write_u32(bytes, offset, bucket.minimum);
		write_u32(bytes, offset + size_of::<u32>(), bucket.maximum);
		write_u64(bytes, offset + size_of::<u32>() * 2, bucket.node_count);
	}
	page.seal()?;
	Ok(page)
}

/// Decodes and validates one bounded degree distribution.
fn decode_degree_histogram(
	page: &AlignedPage,
	page_no: PageNo,
	node_labels: &[String],
	edge_labels: &[String],
) -> FerrosGrynnResult<(DegreeHistogramKey, DegreeHistogram)> {
	let header = page.validate(page_no)?;
	if header.page_type() != PageType::DegreeHistogram || !header.flags().is_empty() {
		return Err(invalid_page(page_no.get(), "invalid DegreeHistogram page header"));
	}
	let bytes = page.as_bytes();
	validate_zero(
		&bytes[DEGREE_STATS_PADDING_OFFSET..DEGREE_STATS_BUCKET_COUNT_OFFSET],
		page_no,
		"degree histogram padding",
	)?;
	let label_id = read_u32(bytes, DEGREE_STATS_LABEL_OFFSET);
	let label = (label_id != NULL_U32)
		.then(|| decoded_label_id(label_id, node_labels, edge_labels))
		.transpose()?;
	if label.as_ref().is_some_and(|label| label.kind != AtomKind::Node) {
		return Err(invalid_page(page_no.get(), "degree histogram uses an edge label"));
	}
	let direction = DegreeDirection::from_byte(bytes[DEGREE_STATS_DIRECTION_OFFSET])
		.ok_or_else(|| invalid_page(page_no.get(), "unknown degree histogram direction"))?;
	let count = usize::from(read_u16(bytes, DEGREE_STATS_BUCKET_COUNT_OFFSET));
	if count > HISTOGRAM_BUCKETS
		|| DEGREE_STATS_BUCKETS_OFFSET + count * DEGREE_STATS_BUCKET_BYTES > PAGE_SIZE
	{
		return Err(invalid_page(page_no.get(), "degree histogram exceeds page capacity"));
	}
	let mut buckets = Vec::with_capacity(count);
	for index in 0..count {
		let offset = DEGREE_STATS_BUCKETS_OFFSET + index * DEGREE_STATS_BUCKET_BYTES;
		let bucket = DegreeBucket {
			minimum: read_u32(bytes, offset),
			maximum: read_u32(bytes, offset + size_of::<u32>()),
			node_count: read_u64(bytes, offset + size_of::<u32>() * 2),
		};
		if bucket.minimum > bucket.maximum || bucket.node_count == 0 {
			return Err(invalid_page(page_no.get(), "invalid degree histogram bucket"));
		}
		if buckets.last().is_some_and(|previous: &DegreeBucket| previous.maximum > bucket.minimum) {
			return Err(invalid_page(page_no.get(), "degree histogram buckets are not ordered"));
		}
		buckets.push(bucket);
	}
	let used = DEGREE_STATS_BUCKETS_OFFSET + count * DEGREE_STATS_BUCKET_BYTES;
	validate_zero(&bytes[used..], page_no, "degree histogram trailing bytes")?;
	Ok((
		DegreeHistogramKey {
			label,
			direction,
		},
		DegreeHistogram {
			buckets,
		},
	))
}

/// Encodes one property distribution, omitting it when it cannot fit one page.
fn encode_property_histogram(
	page_no: PageNo,
	page_lsn: PageLsn,
	key: PropertyHistogramKey,
	histogram: &PropertyHistogram,
) -> FerrosGrynnResult<Option<AlignedPage>> {
	if histogram.buckets.len() > HISTOGRAM_BUCKETS
		|| !histogram.null_fraction.is_finite()
		|| !(0.0..=1.0).contains(&histogram.null_fraction)
		|| !valid_type_tag(histogram.type_tag)
		|| histogram.buckets.is_empty() != (histogram.distinct_count == 0)
		|| key.table_id == NULL_U32
		|| key.field_id == NULL_U32
	{
		return Err(FerrosGrynnError::invalid_storage_file("invalid property histogram metadata"));
	}
	let count = u16::try_from(histogram.buckets.len()).map_err(|_error| {
		FerrosGrynnError::invalid_storage_file("property histogram bucket count exceeds u16")
	})?;
	let bucket_ndv = histogram.buckets.iter().try_fold(0_u64, |total, bucket| {
		total.checked_add(bucket.distinct_count).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("property histogram bucket NDV overflows")
		})
	})?;
	if histogram.distinct_count > bucket_ndv {
		return Err(FerrosGrynnError::invalid_storage_file(
			"property histogram NDV exceeds bucket NDV",
		));
	}
	let mut encoded = Vec::with_capacity(histogram.buckets.len());
	let mut next_offset = PROPERTY_STATS_OFFSETS_OFFSET
		.checked_add(histogram.buckets.len() * size_of::<u64>())
		.ok_or_else(|| FerrosGrynnError::invalid_storage_file("histogram offsets overflow"))?;
	for bucket in &histogram.buckets {
		if bucket.lower > bucket.upper
			|| bucket.frequency == 0
			|| bucket.distinct_count == 0
			|| bucket.distinct_count > bucket.frequency
		{
			return Err(FerrosGrynnError::invalid_storage_file(
				"invalid property histogram bucket",
			));
		}
		let lower_len = u16::try_from(bucket.lower.len()).ok();
		let upper_len = u16::try_from(bucket.upper.len()).ok();
		let Some((lower_len, upper_len)) = lower_len.zip(upper_len) else {
			return Ok(None);
		};
		let bucket_bytes =
			size_of::<u16>() * 2 + bucket.lower.len() + bucket.upper.len() + size_of::<u64>() * 2;
		if next_offset.checked_add(bucket_bytes).is_none_or(|end| end > PAGE_SIZE) {
			return Ok(None);
		}
		encoded.push((next_offset, lower_len, upper_len, bucket));
		next_offset += bucket_bytes;
	}
	let mut page =
		AlignedPage::new(PageType::PropertyHistogram, PageFlags::empty(), page_lsn, page_no);
	let bytes = page.as_bytes_mut();
	write_u32(bytes, PROPERTY_STATS_TABLE_OFFSET, key.table_id);
	write_u32(bytes, PROPERTY_STATS_FIELD_OFFSET, key.field_id);
	bytes[PROPERTY_STATS_TYPE_OFFSET] = histogram.type_tag;
	write_u64(bytes, PROPERTY_STATS_DISTINCT_OFFSET, histogram.distinct_count);
	write_u32(bytes, PROPERTY_STATS_NULL_FRACTION_OFFSET, histogram.null_fraction.to_bits());
	write_u16(bytes, PROPERTY_STATS_BUCKET_COUNT_OFFSET, count);
	for (index, (offset, lower_len, upper_len, bucket)) in encoded.iter().enumerate() {
		let offset_u64 = u64::try_from(*offset).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("property histogram offset exceeds u64")
		})?;
		write_u64(bytes, PROPERTY_STATS_OFFSETS_OFFSET + index * size_of::<u64>(), offset_u64);
		write_u16(bytes, *offset, *lower_len);
		write_u16(bytes, *offset + size_of::<u16>(), *upper_len);
		let mut cursor = *offset + size_of::<u16>() * 2;
		bytes[cursor..cursor + bucket.lower.len()].copy_from_slice(&bucket.lower);
		cursor += bucket.lower.len();
		bytes[cursor..cursor + bucket.upper.len()].copy_from_slice(&bucket.upper);
		cursor += bucket.upper.len();
		write_u64(bytes, cursor, bucket.frequency);
		write_u64(bytes, cursor + size_of::<u64>(), bucket.distinct_count);
	}
	page.seal()?;
	Ok(Some(page))
}

/// Decodes and validates one property distribution.
fn decode_property_histogram(
	page: &AlignedPage,
	page_no: PageNo,
) -> FerrosGrynnResult<(PropertyHistogramKey, PropertyHistogram)> {
	let header = page.validate(page_no)?;
	if header.page_type() != PageType::PropertyHistogram || !header.flags().is_empty() {
		return Err(invalid_page(page_no.get(), "invalid PropertyHistogram page header"));
	}
	let bytes = page.as_bytes();
	let table_id = read_u32(bytes, PROPERTY_STATS_TABLE_OFFSET);
	let field_id = read_u32(bytes, PROPERTY_STATS_FIELD_OFFSET);
	if table_id == NULL_U32 || field_id == NULL_U32 {
		return Err(invalid_page(page_no.get(), "property histogram uses a null schema id"));
	}
	validate_zero(
		&bytes[PROPERTY_STATS_PADDING_OFFSET..PROPERTY_STATS_DISTINCT_OFFSET],
		page_no,
		"property histogram padding",
	)?;
	let type_tag = bytes[PROPERTY_STATS_TYPE_OFFSET];
	if !valid_type_tag(type_tag) {
		return Err(invalid_page(page_no.get(), "unknown property histogram type tag"));
	}
	let null_fraction = f32::from_bits(read_u32(bytes, PROPERTY_STATS_NULL_FRACTION_OFFSET));
	if !null_fraction.is_finite() || !(0.0..=1.0).contains(&null_fraction) {
		return Err(invalid_page(page_no.get(), "invalid property histogram null fraction"));
	}
	let count = usize::from(read_u16(bytes, PROPERTY_STATS_BUCKET_COUNT_OFFSET));
	if count > HISTOGRAM_BUCKETS {
		return Err(invalid_page(page_no.get(), "property histogram exceeds bucket limit"));
	}
	let data_start = PROPERTY_STATS_OFFSETS_OFFSET
		.checked_add(count * size_of::<u64>())
		.ok_or_else(|| invalid_page(page_no.get(), "property histogram offsets overflow"))?;
	if data_start > PAGE_SIZE {
		return Err(invalid_page(page_no.get(), "property histogram offsets exceed page"));
	}
	let offsets = (0..count)
		.map(|index| {
			usize::try_from(read_u64(
				bytes,
				PROPERTY_STATS_OFFSETS_OFFSET + index * size_of::<u64>(),
			))
			.map_err(|_error| invalid_page(page_no.get(), "histogram offset exceeds usize"))
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	if offsets.first().is_some_and(|offset| *offset != data_start)
		|| offsets.windows(2).any(|pair| pair[0] >= pair[1])
		|| offsets.iter().any(|offset| *offset >= PAGE_SIZE)
	{
		return Err(invalid_page(page_no.get(), "invalid property histogram offsets"));
	}
	let mut buckets = Vec::with_capacity(count);
	let mut used = data_start;
	for (index, offset) in offsets.iter().copied().enumerate() {
		let limit = offsets.get(index + 1).copied().unwrap_or(PAGE_SIZE);
		let header_end = offset.checked_add(size_of::<u16>() * 2).ok_or_else(|| {
			invalid_page(page_no.get(), "property histogram bucket header overflows")
		})?;
		if header_end > limit {
			return Err(invalid_page(page_no.get(), "truncated property histogram bucket"));
		}
		let lower_len = usize::from(read_u16(bytes, offset));
		let upper_len = usize::from(read_u16(bytes, offset + size_of::<u16>()));
		let lower_start = header_end;
		let upper_start = lower_start.checked_add(lower_len).ok_or_else(|| {
			invalid_page(page_no.get(), "property histogram lower bound overflows")
		})?;
		let counters = upper_start.checked_add(upper_len).ok_or_else(|| {
			invalid_page(page_no.get(), "property histogram upper bound overflows")
		})?;
		let end = counters.checked_add(size_of::<u64>() * 2).ok_or_else(|| {
			invalid_page(page_no.get(), "property histogram bucket length overflows")
		})?;
		if end > limit || (index + 1 < offsets.len() && end != limit) {
			return Err(invalid_page(page_no.get(), "invalid property histogram bucket length"));
		}
		let bucket = PropertyBucket {
			lower: bytes[lower_start..upper_start].to_vec(),
			upper: bytes[upper_start..counters].to_vec(),
			frequency: read_u64(bytes, counters),
			distinct_count: read_u64(bytes, counters + size_of::<u64>()),
		};
		if bucket.lower > bucket.upper
			|| bucket.frequency == 0
			|| bucket.distinct_count == 0
			|| bucket.distinct_count > bucket.frequency
			|| buckets.last().is_some_and(|previous: &PropertyBucket| previous.upper > bucket.lower)
		{
			return Err(invalid_page(page_no.get(), "invalid property histogram bucket"));
		}
		used = end;
		buckets.push(bucket);
	}
	validate_zero(&bytes[used..], page_no, "property histogram trailing bytes")?;
	let distinct_count = read_u64(bytes, PROPERTY_STATS_DISTINCT_OFFSET);
	if buckets.is_empty() != (distinct_count == 0) {
		return Err(invalid_page(
			page_no.get(),
			"property histogram NDV disagrees with its buckets",
		));
	}
	let bucket_ndv = buckets.iter().try_fold(0_u64, |total, bucket| {
		total
			.checked_add(bucket.distinct_count)
			.ok_or_else(|| invalid_page(page_no.get(), "property histogram bucket NDV overflows"))
	})?;
	if distinct_count > bucket_ndv {
		return Err(invalid_page(page_no.get(), "property histogram NDV exceeds bucket NDV"));
	}
	Ok((
		PropertyHistogramKey {
			table_id,
			field_id,
		},
		PropertyHistogram {
			type_tag,
			distinct_count,
			null_fraction,
			buckets,
		},
	))
}

/// Returns whether a byte is a closed format-v1 histogram type tag.
fn valid_type_tag(value: u8) -> bool {
	matches!(
		value,
		0x00..=0x0c | 0x10..=0x13 | 0x15 | 0x21..=0x27 | 0x31..=0x38
	)
}

/// Recovers the integer total from a legacy one-page average.
fn legacy_total(average: f64, count: u64, name: &'static str) -> FerrosGrynnResult<u64> {
	let total = average * count as f64;
	if !total.is_finite() || total > u64::MAX as f64 {
		return Err(invalid_page(0, format!("legacy {name} total exceeds u64")));
	}
	Ok(total.round() as u64)
}

/// Compares redundant persisted means with scale-aware floating tolerance.
fn approximately_equal(left: f64, right: f64) -> bool {
	(left - right).abs() <= f64::EPSILON * left.abs().max(right.abs()).max(1.0) * 4.0
}

/// Narrows an auxiliary-page count to its root directory width.
fn checked_page_count(value: usize) -> FerrosGrynnResult<u32> {
	u32::try_from(value).map_err(|_error| {
		FerrosGrynnError::invalid_storage_file("statistics page count exceeds u32")
	})
}

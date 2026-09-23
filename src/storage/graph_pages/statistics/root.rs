//! Eager graph-statistics root and its split-file directory.

use super::codec::*;
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	stats::GraphStatistics,
	storage::page::{AlignedPage, PAGE_SIZE, PageFlags, PageLsn, PageNo, PageType},
};

const LAYOUT_VERSION: u32 = 1;
const NODE_COUNT: usize = 32;
const EDGE_COUNT: usize = 40;
const AVG_OUT: usize = 48;
const AVG_IN: usize = 56;
const MAX_OUT: usize = 64;
const MAX_IN: usize = 72;
const DEGREE_FIRST: usize = 80;
const LABEL_FIRST: usize = 88;
const PROPERTY_FIRST: usize = 96;
const LABEL_PAGES: usize = 104;
const DEGREE_PAGES: usize = 108;
const PROPERTY_PAGES: usize = 112;
const VERSION: usize = 116;
const ANALYZED_REVISION: usize = 120;
const DIRTY_ROWS: usize = 128;
const TOTAL_OUT: usize = 136;
const TOTAL_IN: usize = 144;
const TOTAL_UNDIRECTED: usize = 152;
const REVISION: usize = 160;
const CLASS_FIRST: usize = 168;
const CLASS_PAGES: usize = 176;
const CLASS_RESERVED: usize = 180;
const RESERVED: usize = 184;

/// Exact page ranges declared by graph statistics page zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::storage) struct StatisticsLayout {
	pub(super) degree_first: Option<PageNo>,
	pub(super) label_first: Option<PageNo>,
	pub(super) property_first: Option<PageNo>,
	pub(super) class_first: Option<PageNo>,
	pub(super) degree_pages: u32,
	pub(super) label_pages: u32,
	pub(super) property_pages: u32,
	pub(super) class_pages: u32,
}

impl StatisticsLayout {
	pub(super) fn from_counts(
		degree: usize,
		classes: usize,
		labels: usize,
		properties: usize,
	) -> FerrosGrynnResult<Self> {
		let degree_pages = count(degree, "degree")?;
		let class_pages = count(classes, "class")?;
		let label_pages = count(labels, "label")?;
		let property_pages = count(properties, "property")?;
		let class_start = 1_u64
			.checked_add(u64::from(degree_pages))
			.ok_or_else(|| invalid("class statistics range start overflows"))?;
		Ok(Self {
			degree_first: (degree_pages != 0).then(|| PageNo::new(1)),
			label_first: (label_pages != 0).then(|| PageNo::new(0)),
			property_first: (property_pages != 0).then(|| PageNo::new(0)),
			class_first: (class_pages != 0).then(|| PageNo::new(class_start)),
			degree_pages,
			label_pages,
			property_pages,
			class_pages,
		})
	}

	pub(super) fn graph_len(self) -> u64 {
		1 + u64::from(self.degree_pages) + u64::from(self.class_pages)
	}

	pub(super) const fn label_len(self) -> u64 {
		self.label_pages as u64
	}

	pub(super) const fn property_len(self) -> u64 {
		self.property_pages as u64
	}
}

pub(super) fn encode_statistics_root(
	page: &mut AlignedPage,
	statistics: &GraphStatistics,
	layout: StatisticsLayout,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<()> {
	validate_summary(statistics)?;
	validate_starts(layout)?;
	page.reset(PageType::StatisticsRoot, PageFlags::empty(), page_lsn, PageNo::new(0));
	let bytes = page.as_bytes_mut();
	write_u64(bytes, NODE_COUNT, statistics.node_count);
	write_u64(bytes, EDGE_COUNT, statistics.edge_count);
	write_u64(bytes, AVG_OUT, statistics.average_out_degree().to_bits());
	write_u64(bytes, AVG_IN, statistics.average_in_degree().to_bits());
	write_u64(bytes, MAX_OUT, statistics.maximum_out_degree);
	write_u64(bytes, MAX_IN, statistics.maximum_in_degree);
	write_optional_page(bytes, DEGREE_FIRST, layout.degree_first);
	write_optional_page(bytes, LABEL_FIRST, layout.label_first);
	write_optional_page(bytes, PROPERTY_FIRST, layout.property_first);
	write_u32(bytes, LABEL_PAGES, layout.label_pages);
	write_u32(bytes, DEGREE_PAGES, layout.degree_pages);
	write_u32(bytes, PROPERTY_PAGES, layout.property_pages);
	write_u32(bytes, VERSION, LAYOUT_VERSION);
	write_u64(bytes, ANALYZED_REVISION, statistics.analyzed_revision);
	write_u64(bytes, DIRTY_ROWS, statistics.dirty_rows);
	write_u64(bytes, TOTAL_OUT, statistics.total_out_degree);
	write_u64(bytes, TOTAL_IN, statistics.total_in_degree);
	write_u64(bytes, TOTAL_UNDIRECTED, statistics.total_undirected_degree);
	write_u64(bytes, REVISION, statistics.revision);
	write_optional_page(bytes, CLASS_FIRST, layout.class_first);
	write_u32(bytes, CLASS_PAGES, layout.class_pages);
	write_u32(bytes, CLASS_RESERVED, 0);
	page.seal()
}

/// Decodes the eager root and validates the exact physical file lengths.
pub(in crate::storage) fn decode_statistics_root(
	page: &AlignedPage,
	graph_pages: u64,
	label_pages: u64,
	property_pages: u64,
) -> FerrosGrynnResult<(GraphStatistics, StatisticsLayout)> {
	let page_no = PageNo::new(0);
	let header = page.validate(page_no)?;
	if header.page_type() != PageType::StatisticsRoot || !header.flags().is_empty() {
		return Err(invalid_page(page_no, "invalid StatisticsRoot page header"));
	}
	let bytes = page.as_bytes();
	if read_u32(bytes, VERSION) != LAYOUT_VERSION {
		return Err(FerrosGrynnError::unsupported_storage_format(
			u64::from(read_u32(bytes, VERSION)),
			u64::from(LAYOUT_VERSION),
		));
	}
	validate_zero(
		&bytes[CLASS_RESERVED..RESERVED],
		page_no,
		"statistics class-range reserved bytes",
	)?;
	validate_zero(&bytes[RESERVED..PAGE_SIZE], page_no, "statistics root reserved bytes")?;
	let statistics = GraphStatistics {
		revision: read_u64(bytes, REVISION),
		analyzed_revision: read_u64(bytes, ANALYZED_REVISION),
		dirty_rows: read_u64(bytes, DIRTY_ROWS),
		node_count: read_u64(bytes, NODE_COUNT),
		edge_count: read_u64(bytes, EDGE_COUNT),
		total_out_degree: read_u64(bytes, TOTAL_OUT),
		total_in_degree: read_u64(bytes, TOTAL_IN),
		total_undirected_degree: read_u64(bytes, TOTAL_UNDIRECTED),
		maximum_out_degree: read_u64(bytes, MAX_OUT),
		maximum_in_degree: read_u64(bytes, MAX_IN),
		..GraphStatistics::default()
	};
	validate_summary(&statistics)?;
	for (stored, derived, name) in [
		(f64::from_bits(read_u64(bytes, AVG_OUT)), statistics.average_out_degree(), "out"),
		(f64::from_bits(read_u64(bytes, AVG_IN)), statistics.average_in_degree(), "in"),
	] {
		if !stored.is_finite() || stored < 0.0 || !approximately_equal(stored, derived) {
			return Err(invalid_page(page_no, format!("{name}-degree average is invalid")));
		}
	}
	let layout = StatisticsLayout {
		degree_first: read_optional_page(bytes, DEGREE_FIRST),
		label_first: read_optional_page(bytes, LABEL_FIRST),
		property_first: read_optional_page(bytes, PROPERTY_FIRST),
		class_first: read_optional_page(bytes, CLASS_FIRST),
		degree_pages: read_u32(bytes, DEGREE_PAGES),
		label_pages: read_u32(bytes, LABEL_PAGES),
		property_pages: read_u32(bytes, PROPERTY_PAGES),
		class_pages: read_u32(bytes, CLASS_PAGES),
	};
	validate_statistics_file_lengths(layout, graph_pages, label_pages, property_pages)?;
	Ok((statistics, layout))
}

pub(in crate::storage) fn validate_statistics_file_lengths(
	layout: StatisticsLayout,
	graph_pages: u64,
	label_pages: u64,
	property_pages: u64,
) -> FerrosGrynnResult<()> {
	validate_starts(layout)?;
	if layout.graph_len() != graph_pages
		|| layout.label_len() != label_pages
		|| layout.property_len() != property_pages
	{
		return Err(invalid("statistics directory disagrees with physical file lengths"));
	}
	Ok(())
}

pub(super) fn validate_summary(statistics: &GraphStatistics) -> FerrosGrynnResult<()> {
	if statistics.analyzed_revision > statistics.revision {
		return Err(invalid("statistics analysis revision exceeds exact revision"));
	}
	if statistics.node_count == 0
		&& (statistics.total_out_degree != 0
			|| statistics.total_in_degree != 0
			|| statistics.total_undirected_degree != 0
			|| statistics.maximum_out_degree != 0
			|| statistics.maximum_in_degree != 0)
	{
		return Err(invalid("empty graph has non-zero degree statistics"));
	}
	if statistics.maximum_out_degree > statistics.total_out_degree
		|| statistics.maximum_in_degree > statistics.total_in_degree
	{
		return Err(invalid("maximum degree exceeds its exact total"));
	}
	Ok(())
}

fn validate_starts(layout: StatisticsLayout) -> FerrosGrynnResult<()> {
	for (first, pages, expected, name) in [
		(layout.degree_first, layout.degree_pages, 1, "degree"),
		(layout.label_first, layout.label_pages, 0, "label"),
		(layout.property_first, layout.property_pages, 0, "property"),
	] {
		match (first, pages) {
			(None, 0) => {}
			(Some(page), count) if count != 0 && page.get() == expected => {}
			_ => return Err(invalid(format!("statistics {name} range start is invalid"))),
		}
	}
	let class_start = 1_u64
		.checked_add(u64::from(layout.degree_pages))
		.ok_or_else(|| invalid("class statistics range start overflows"))?;
	match (layout.class_first, layout.class_pages) {
		(None, 0) => {}
		(Some(page), count) if count != 0 && page.get() == class_start => {}
		_ => return Err(invalid("statistics class range start is invalid")),
	}
	Ok(())
}

fn count(value: usize, family: &str) -> FerrosGrynnResult<u32> {
	u32::try_from(value).map_err(|_error| invalid(format!("{family} statistics exceed u32 pages")))
}

fn approximately_equal(left: f64, right: f64) -> bool {
	(left - right).abs() <= f64::EPSILON * left.abs().max(right.abs()).max(1.0) * 4.0
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

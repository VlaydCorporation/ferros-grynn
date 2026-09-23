//! Database-global Base statistics split by access frequency.

mod root;

#[cfg(test)]
mod tests;

pub(super) use root::{BaseStatisticsLayout, decode_base_statistics_root};

use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	stats::{BaseStatistics, StatisticsScope},
	storage::{
		graph_pages::{
			decode_class_statistics_range,
			decode_property_statistics_range,
			encode_class_statistics,
			encode_property_statistics,
		},
		page::{AlignedPage, PageLsn, PageNo},
	},
};

/// Complete database-global statistics image.
pub(super) struct EncodedBaseStatisticsFiles {
	/// Eager Base root followed by exact per-class counters.
	pub(super) classes: Vec<AlignedPage>,
	/// Independently loaded Base property histograms.
	pub(super) properties: Vec<AlignedPage>,
}

/// Encodes the two database-global statistics files.
pub(super) fn encode_base_statistics(
	statistics: &BaseStatistics,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<EncodedBaseStatisticsFiles> {
	if statistics.property_histograms.keys().any(|key| key.scope != StatisticsScope::Base) {
		return Err(invalid("Base statistics contain a graph-effective histogram"));
	}
	let mut class_pages = encode_class_statistics(&statistics.classes, 1, page_lsn)?;
	let properties = encode_property_statistics(&statistics.property_histograms, page_lsn)?;
	let layout = BaseStatisticsLayout::from_counts(class_pages.len(), properties.len())?;
	let capacity = class_pages
		.len()
		.checked_add(1)
		.ok_or_else(|| invalid("Base class-statistics file length overflows usize"))?;
	let mut classes = Vec::new();
	classes
		.try_reserve_exact(capacity)
		.map_err(|_error| invalid("Base class-statistics allocation failed"))?;
	// Initialize the over-aligned root in its final allocation. Moving a
	// populated 16 KiB page was observed to corrupt its first byte on MSVC.
	classes.push(AlignedPage::zeroed());
	root::encode_base_statistics_root(&mut classes[0], statistics, layout, page_lsn)?;
	classes.append(&mut class_pages);
	Ok(EncodedBaseStatisticsFiles {
		classes,
		properties,
	})
}

/// Loads the exact per-class Base counters declared by an eager root.
pub(super) fn decode_base_class_statistics<ReadPage>(
	read_page: ReadPage,
	layout: BaseStatisticsLayout,
) -> FerrosGrynnResult<std::collections::BTreeMap<crate::datastore::ClassId, u64>>
where
	ReadPage: FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
{
	decode_class_statistics_range(read_page, layout.class_first, layout.class_pages)
}

/// Loads the Base property histograms declared by an eager root.
pub(super) fn decode_base_property_statistics<ReadPage>(
	read_page: ReadPage,
	layout: BaseStatisticsLayout,
) -> FerrosGrynnResult<
	std::collections::BTreeMap<crate::stats::PropertyHistogramKey, crate::stats::PropertyHistogram>,
>
where
	ReadPage: FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
{
	let properties = decode_property_statistics_range(read_page, layout.property_pages)?;
	if properties.keys().any(|key| key.scope != StatisticsScope::Base) {
		return Err(invalid("Base statistics file contains a graph-effective histogram"));
	}
	Ok(properties)
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

use super::*;
#[cfg(test)]
use crate::test_assertions::AssertOption as _;
use crate::{
	datastore::{ClassId, SchemaId},
	stats::{HistogramMethod, PropertyBucket, PropertyHistogram, PropertyHistogramKey},
	test_assertions::AssertResult as _,
};

/// Verifies base statistics round trip keeps split ranges.
#[test]
fn base_statistics_round_trip_keeps_split_ranges() {
	use crate::test_assertions::AssertResult as _;

	let class = ClassId::new(7).assert_some();
	let mut statistics = BaseStatistics {
		revision: 9,
		analyzed_revision: 8,
		dirty_rows: 1,
		..BaseStatistics::default()
	};
	statistics.classes.insert(class, 3);
	let histogram_key = PropertyHistogramKey {
		class_id: class,
		field_id: SchemaId::new(8).assert_some(),
		scope: StatisticsScope::Base,
	};
	statistics.property_histograms.insert(histogram_key, PropertyHistogram {
		mbr_sample: Vec::new(),
		mbr_population: 0,
		type_tag: crate::values::storage::TypeTag::Int.byte(),
		method: HistogramMethod::Exact,
		sample_count: 3,
		distinct_count: 1,
		null_fraction: 0.0,
		buckets: vec![PropertyBucket {
			lower: vec![1],
			upper: vec![1],
			frequency: 3,
			distinct_count: 1,
		}],
	});

	let files = encode_base_statistics(&statistics, PageLsn::new(4)).assert_ok();
	let (mut decoded, layout) = decode_base_statistics_root(
		&files.classes[0],
		files.classes.len() as u64,
		files.properties.len() as u64,
	)
	.assert_ok();
	decoded.classes = decode_base_class_statistics(
		|page_no, output| copy_page(&files.classes, page_no, output),
		layout,
	)
	.assert_ok();
	decoded.property_histograms = decode_base_property_statistics(
		|page_no, output| copy_page(&files.properties, page_no, output),
		layout,
	)
	.assert_ok();

	assert_eq!(decoded, statistics);
}

/// Verifies base statistics root rejects extra pages.
#[test]
fn base_statistics_root_rejects_extra_pages() {
	use crate::test_assertions::AssertResult as _;

	let files = encode_base_statistics(&BaseStatistics::default(), PageLsn::ZERO).assert_ok();
	assert!(decode_base_statistics_root(&files.classes[0], 2, 0).is_err());
}

/// Returns copy page for this module.
fn copy_page(
	pages: &[AlignedPage],
	page_no: PageNo,
	output: &mut AlignedPage,
) -> FerrosGrynnResult<()> {
	let page = pages
		.get(usize::try_from(page_no.get()).assert_ok())
		.ok_or_else(|| invalid("test page is absent"))?;
	output.as_bytes_mut().copy_from_slice(page.as_bytes());
	Ok(())
}

use super::*;
use crate::{
	datastore::{ClassId, SchemaId},
	stats::{HistogramMethod, PropertyBucket, PropertyHistogram, PropertyHistogramKey},
};

#[test]
fn base_statistics_round_trip_keeps_split_ranges() -> FerrosGrynnResult<()> {
	let class = ClassId::new(7).unwrap();
	let mut statistics = BaseStatistics {
		revision: 9,
		analyzed_revision: 8,
		dirty_rows: 1,
		..BaseStatistics::default()
	};
	statistics.classes.insert(class, 3);
	let histogram_key = PropertyHistogramKey {
		class_id: class,
		field_id: SchemaId::new(8).unwrap(),
		scope: StatisticsScope::Base,
	};
	statistics.property_histograms.insert(histogram_key, PropertyHistogram {
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

	let files = encode_base_statistics(&statistics, PageLsn::new(4))?;
	let (mut decoded, layout) = decode_base_statistics_root(
		&files.classes[0],
		files.classes.len() as u64,
		files.properties.len() as u64,
	)?;
	decoded.classes = decode_base_class_statistics(
		|page_no, output| copy_page(&files.classes, page_no, output),
		layout,
	)?;
	decoded.property_histograms = decode_base_property_statistics(
		|page_no, output| copy_page(&files.properties, page_no, output),
		layout,
	)?;

	assert_eq!(decoded, statistics);
	Ok(())
}

#[test]
fn base_statistics_root_rejects_extra_pages() -> FerrosGrynnResult<()> {
	let files = encode_base_statistics(&BaseStatistics::default(), PageLsn::ZERO)?;
	assert!(decode_base_statistics_root(&files.classes[0], 2, 0).is_err());
	Ok(())
}

fn copy_page(
	pages: &[AlignedPage],
	page_no: PageNo,
	output: &mut AlignedPage,
) -> FerrosGrynnResult<()> {
	let page = pages
		.get(usize::try_from(page_no.get()).unwrap())
		.ok_or_else(|| invalid("test page is absent"))?;
	output.as_bytes_mut().copy_from_slice(page.as_bytes());
	Ok(())
}

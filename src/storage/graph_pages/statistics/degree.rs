//! Degree-histogram pages stored after the eager statistics root.

use std::collections::BTreeMap;

use super::{checked_page_no, codec::*};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::AtomKind,
	stats::{
		DegreeBucket,
		DegreeDirection,
		DegreeHistogram,
		DegreeHistogramKey,
		HISTOGRAM_BUCKETS,
	},
	storage::{
		graph_index::{LabelDictionary, LabelKind},
		page::{AlignedPage, PAGE_SIZE, PageFlags, PageLsn, PageNo, PageType},
	},
};

const KIND: usize = 32;
const DIRECTION: usize = 33;
const BUCKET_COUNT: usize = 34;
const LABEL_ID: usize = 36;
const BUCKETS: usize = 40;
const BUCKET_BYTES: usize = 16;
const NULL_LABEL: u32 = u32::MAX;

pub(super) fn encode_degree_statistics(
	statistics: &crate::stats::GraphStatistics,
	dictionary: &LabelDictionary,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<Vec<AlignedPage>> {
	let mut pages = Vec::new();
	pages
		.try_reserve_exact(statistics.degree_histograms.len())
		.map_err(|_error| invalid("degree statistics allocation failed"))?;
	for (index, (key, histogram)) in statistics.degree_histograms.iter().enumerate() {
		let page_no = checked_page_no(index, 1, "degree statistics")?;
		pages.push(encode_page(page_no, page_lsn, key, histogram, dictionary)?);
	}
	Ok(pages)
}

pub(in crate::storage) fn decode_degree_statistics<ReadPage>(
	mut read_page: ReadPage,
	layout: super::StatisticsLayout,
	dictionary: &LabelDictionary,
) -> FerrosGrynnResult<BTreeMap<DegreeHistogramKey, DegreeHistogram>>
where
	ReadPage: FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
{
	let mut result = BTreeMap::new();
	let mut page = AlignedPage::zeroed();
	for index in 0..usize::try_from(layout.degree_pages)
		.map_err(|_error| invalid("degree statistics page count exceeds usize"))?
	{
		let page_no = checked_page_no(index, 1, "degree statistics")?;
		read_page(page_no, &mut page)?;
		let (key, histogram) = decode_page(&page, page_no, dictionary)?;
		if result.insert(key, histogram).is_some() {
			return Err(invalid_page(page_no, "duplicate degree histogram"));
		}
	}
	Ok(result)
}

fn encode_page(
	page_no: PageNo,
	page_lsn: PageLsn,
	key: &DegreeHistogramKey,
	histogram: &DegreeHistogram,
	dictionary: &LabelDictionary,
) -> FerrosGrynnResult<AlignedPage> {
	validate_buckets(&histogram.buckets)?;
	let label_id = match &key.label {
		None => NULL_LABEL,
		Some(label) if label.kind == AtomKind::Node => dictionary
			.id(LabelKind::Node, label.label.as_str())
			.ok_or_else(|| invalid("degree histogram label is absent from dictionary"))?,
		Some(_) => return Err(invalid("degree histograms cannot use edge labels")),
	};
	let count = u16::try_from(histogram.buckets.len())
		.map_err(|_error| invalid("degree histogram bucket count exceeds u16"))?;
	let mut page =
		AlignedPage::new(PageType::DegreeHistogram, PageFlags::empty(), page_lsn, page_no);
	let bytes = page.as_bytes_mut();
	bytes[KIND] = 0;
	bytes[DIRECTION] = key.direction as u8;
	write_u16(bytes, BUCKET_COUNT, count);
	write_u32(bytes, LABEL_ID, label_id);
	for (index, bucket) in histogram.buckets.iter().enumerate() {
		let offset = BUCKETS + index * BUCKET_BYTES;
		write_u32(bytes, offset, bucket.minimum);
		write_u32(bytes, offset + 4, bucket.maximum);
		write_u64(bytes, offset + 8, bucket.node_count);
	}
	page.seal()?;
	Ok(page)
}

fn decode_page(
	page: &AlignedPage,
	page_no: PageNo,
	dictionary: &LabelDictionary,
) -> FerrosGrynnResult<(DegreeHistogramKey, DegreeHistogram)> {
	let header = page.validate(page_no)?;
	if header.page_type() != PageType::DegreeHistogram || !header.flags().is_empty() {
		return Err(invalid_page(page_no, "invalid DegreeHistogram page header"));
	}
	let bytes = page.as_bytes();
	if bytes[KIND] != 0 {
		return Err(invalid_page(page_no, "degree histogram kind is not NodeLabel"));
	}
	let direction = DegreeDirection::from_byte(bytes[DIRECTION])
		.ok_or_else(|| invalid_page(page_no, "unknown degree direction"))?;
	let count = usize::from(read_u16(bytes, BUCKET_COUNT));
	if count > HISTOGRAM_BUCKETS || BUCKETS + count * BUCKET_BYTES > PAGE_SIZE {
		return Err(invalid_page(page_no, "degree histogram exceeds page capacity"));
	}
	let label_id = read_u32(bytes, LABEL_ID);
	let label = if label_id == NULL_LABEL {
		None
	} else {
		Some(crate::stats::LabelStatisticKey {
			kind: AtomKind::Node,
			label: dictionary
				.name(LabelKind::Node, label_id)
				.ok_or_else(|| invalid_page(page_no, "degree label id is out of range"))?
				.into(),
		})
	};
	let mut buckets = Vec::with_capacity(count);
	for index in 0..count {
		let offset = BUCKETS + index * BUCKET_BYTES;
		buckets.push(DegreeBucket {
			minimum: read_u32(bytes, offset),
			maximum: read_u32(bytes, offset + 4),
			node_count: read_u64(bytes, offset + 8),
		});
	}
	validate_buckets(&buckets)?;
	validate_zero(
		&bytes[BUCKETS + count * BUCKET_BYTES..],
		page_no,
		"degree histogram trailing bytes",
	)?;
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

fn validate_buckets(buckets: &[DegreeBucket]) -> FerrosGrynnResult<()> {
	if buckets.len() > HISTOGRAM_BUCKETS
		|| buckets.iter().any(|bucket| bucket.minimum > bucket.maximum || bucket.node_count == 0)
		|| buckets.windows(2).any(|pair| pair[0].maximum > pair[1].minimum)
	{
		return Err(invalid("degree histogram buckets are invalid"));
	}
	Ok(())
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

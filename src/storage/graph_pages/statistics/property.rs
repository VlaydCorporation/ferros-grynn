//! Scope-aware property histograms in their independently lazy file.

use std::collections::BTreeMap;

use super::{checked_page_no, codec::*};
use crate::{
	datastore::{ClassId, SchemaId},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::GraphId,
	stats::{
		HISTOGRAM_BUCKETS,
		HistogramMethod,
		PropertyBucket,
		PropertyHistogram,
		PropertyHistogramKey,
		StatisticsScope,
	},
	storage::page::{AlignedPage, PAGE_SIZE, PageFlags, PageLsn, PageNo, PageType},
};

const CLASS_ID: usize = 32;
const FIELD_ID: usize = 36;
const SCOPE_KIND: usize = 40;
const VALUE_TYPE: usize = 41;
const METHOD: usize = 42;
const HLL_PRECISION: usize = 43;
const SAMPLE_COUNT: usize = 44;
const SCOPE_GRAPH: usize = 48;
const DISTINCT_COUNT: usize = 56;
const NULL_FRACTION: usize = 64;
const BUCKET_COUNT: usize = 68;
const RESERVED: usize = 70;
const OFFSETS: usize = 72;
const GEOMETRY_POPULATION: usize = 72;
const GEOMETRY_OFFSETS: usize = 80;
const EXACT_PRECISION: u8 = 0;
const APPROXIMATE_PRECISION: u8 = 12;

pub(in crate::storage) fn encode_property_statistics(
	histograms: &BTreeMap<PropertyHistogramKey, PropertyHistogram>,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<Vec<AlignedPage>> {
	let capacity = histograms.values().try_fold(0_usize, |total, histogram| {
		validate_histogram(histogram)?;
		total
			.checked_add(if histogram.type_tag == 0x27 {
				histogram.mbr_sample.len().div_ceil(509).max(1)
			} else {
				1
			})
			.ok_or_else(|| invalid("property statistics page count overflows"))
	})?;
	let mut pages = Vec::with_capacity(capacity);
	for (key, histogram) in histograms {
		if histogram.type_tag == 0x27 {
			encode_geometry_sample(&mut pages, page_lsn, *key, histogram)?;
			continue;
		}
		let page_no = checked_page_no(pages.len(), 0, "property statistics")?;
		if let Some(page) = encode_page(page_no, page_lsn, *key, histogram)? {
			pages.push(page);
		}
	}
	Ok(pages)
}

pub(in crate::storage) fn decode_property_statistics<ReadPage>(
	read_page: ReadPage,
	layout: super::StatisticsLayout,
) -> FerrosGrynnResult<BTreeMap<PropertyHistogramKey, PropertyHistogram>>
where
	ReadPage: FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
{
	decode_property_statistics_range(read_page, layout.property_pages)
}

pub(in crate::storage) fn decode_property_statistics_range<ReadPage>(
	mut read_page: ReadPage,
	page_count: u32,
) -> FerrosGrynnResult<BTreeMap<PropertyHistogramKey, PropertyHistogram>>
where
	ReadPage: FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
{
	let count = usize::try_from(page_count)
		.map_err(|_error| invalid("property statistics page count exceeds usize"))?;
	let mut result = BTreeMap::<PropertyHistogramKey, PropertyHistogram>::new();
	let mut geometry_counts = BTreeMap::new();
	let mut page = AlignedPage::zeroed();
	for index in 0..count {
		let page_no = checked_page_no(index, 0, "property statistics")?;
		read_page(page_no, &mut page)?;
		let (key, histogram) = decode_page(&page, page_no)?;
		if histogram.type_tag == 0x27 {
			let ordinal = usize::from(read_u16(page.as_bytes(), RESERVED));
			let expected = usize::try_from(read_u64(page.as_bytes(), DISTINCT_COUNT))
				.map_err(|_| invalid("geometry sample count exceeds usize"))?;
			if expected > 1024
				|| ordinal >= expected.div_ceil(509).max(1)
				|| histogram.mbr_sample.len() != expected.saturating_sub(ordinal * 509).min(509)
			{
				return Err(invalid(
					"geometry sample exceeds its bound or has an incomplete chunk",
				));
			}
			if let Some(previous) = result.get_mut(&key) {
				if ordinal == 0
					|| previous.type_tag != 0x27
					|| ordinal * 509 != previous.mbr_sample.len()
					|| previous.sample_count != histogram.sample_count
					|| previous.mbr_population != histogram.mbr_population
					|| previous.null_fraction != histogram.null_fraction
					|| geometry_counts.get(&key) != Some(&expected)
					|| previous
						.mbr_sample
						.len()
						.checked_add(histogram.mbr_sample.len())
						.is_none_or(|length| length > expected)
				{
					return Err(invalid("inconsistent geometry sample continuation"));
				}
				previous.mbr_sample.extend(histogram.mbr_sample);
			} else {
				if ordinal != 0 || histogram.mbr_sample.len() > expected {
					return Err(invalid("invalid geometry sample root"));
				}
				geometry_counts.insert(key, expected);
				result.insert(key, histogram);
			}
			continue;
		}
		if result.insert(key, histogram).is_some() {
			return Err(invalid_page(page_no, "duplicate property histogram"));
		}
	}
	for (key, expected) in geometry_counts {
		if result.get(&key).is_none_or(|histogram| histogram.mbr_sample.len() != expected) {
			return Err(invalid("truncated geometry sample"));
		}
	}
	Ok(result)
}

/// Stores the bounded sample in final aligned slots, with checked continuation
/// ordinals.
fn encode_geometry_sample(
	pages: &mut Vec<AlignedPage>,
	page_lsn: PageLsn,
	key: PropertyHistogramKey,
	histogram: &PropertyHistogram,
) -> FerrosGrynnResult<()> {
	validate_histogram(histogram)?;
	let start = pages.len();
	let count = histogram.mbr_sample.len().div_ceil(509).max(1);
	pages.try_reserve_exact(count).map_err(|cause| {
		invalid("geometry statistics allocation failed").with_technical_source(cause)
	})?;
	pages.resize_with(start + count, AlignedPage::zeroed);
	for ordinal in 0..count {
		let page_no = checked_page_no(start + ordinal, 0, "geometry statistics")?;
		let page = &mut pages[start + ordinal];
		page.reset(PageType::PropertyHistogram, PageFlags::empty(), page_lsn, page_no);
		let bytes = page.as_bytes_mut();
		write_u32(bytes, CLASS_ID, key.class_id.get());
		write_u32(bytes, FIELD_ID, key.field_id.get());
		if let StatisticsScope::Effective {
			graph,
		} = key.scope
		{
			bytes[SCOPE_KIND] = 1;
			write_u64(bytes, SCOPE_GRAPH, graph.get());
		}
		bytes[VALUE_TYPE] = 0x27;
		write_u32(bytes, SAMPLE_COUNT, histogram.sample_count);
		write_u64(bytes, DISTINCT_COUNT, histogram.mbr_sample.len() as u64);
		write_u64(bytes, GEOMETRY_POPULATION, histogram.mbr_population);
		write_u32(bytes, NULL_FRACTION, histogram.null_fraction.to_bits());
		let chunk = &histogram.mbr_sample[(ordinal * 509).min(histogram.mbr_sample.len())
			..((ordinal + 1) * 509).min(histogram.mbr_sample.len())];
		write_u16(bytes, BUCKET_COUNT, chunk.len() as u16);
		write_u16(bytes, RESERVED, ordinal as u16);
		for (slot, mbr) in chunk.iter().enumerate() {
			for (axis, value) in mbr.iter().enumerate() {
				write_u64(bytes, GEOMETRY_OFFSETS + slot * 32 + axis * 8, value.to_bits());
			}
		}
		page.seal()?;
	}
	Ok(())
}

fn encode_page(
	page_no: PageNo,
	page_lsn: PageLsn,
	key: PropertyHistogramKey,
	histogram: &PropertyHistogram,
) -> FerrosGrynnResult<Option<AlignedPage>> {
	validate_histogram(histogram)?;
	let (scope_kind, scope_graph) = match key.scope {
		StatisticsScope::Base => (0, 0),
		StatisticsScope::Effective {
			graph,
		} => (1, graph.get()),
	};
	let count = u16::try_from(histogram.buckets.len())
		.map_err(|_error| invalid("property histogram bucket count exceeds u16"))?;
	let mut encoded = Vec::with_capacity(histogram.buckets.len());
	let mut next = OFFSETS
		.checked_add(histogram.buckets.len() * size_of::<u64>())
		.ok_or_else(|| invalid("property histogram offset table overflows"))?;
	for bucket in &histogram.buckets {
		let lower = match u16::try_from(bucket.lower.len()) {
			Ok(value) => value,
			Err(_error) => return Ok(None),
		};
		let upper = match u16::try_from(bucket.upper.len()) {
			Ok(value) => value,
			Err(_error) => return Ok(None),
		};
		let width = 4_usize
			.checked_add(bucket.lower.len())
			.and_then(|value| value.checked_add(bucket.upper.len()))
			.and_then(|value| value.checked_add(16))
			.ok_or_else(|| invalid("property histogram bucket width overflows"))?;
		if next.checked_add(width).is_none_or(|end| end > PAGE_SIZE) {
			return Ok(None);
		}
		encoded.push((next, lower, upper, bucket));
		next += width;
	}
	let mut page =
		AlignedPage::new(PageType::PropertyHistogram, PageFlags::empty(), page_lsn, page_no);
	let bytes = page.as_bytes_mut();
	write_u32(bytes, CLASS_ID, key.class_id.get());
	write_u32(bytes, FIELD_ID, key.field_id.get());
	bytes[SCOPE_KIND] = scope_kind;
	bytes[VALUE_TYPE] = histogram.type_tag;
	bytes[METHOD] = histogram.method as u8;
	bytes[HLL_PRECISION] = match histogram.method {
		HistogramMethod::Exact => EXACT_PRECISION,
		HistogramMethod::Approximate => APPROXIMATE_PRECISION,
	};
	write_u32(bytes, SAMPLE_COUNT, histogram.sample_count);
	write_u64(bytes, SCOPE_GRAPH, scope_graph);
	write_u64(bytes, DISTINCT_COUNT, histogram.distinct_count);
	write_u32(bytes, NULL_FRACTION, histogram.null_fraction.to_bits());
	write_u16(bytes, BUCKET_COUNT, count);
	for (index, (offset, lower, upper, bucket)) in encoded.iter().enumerate() {
		write_u64(
			bytes,
			OFFSETS + index * size_of::<u64>(),
			u64::try_from(*offset).map_err(|_error| invalid("bucket offset exceeds u64"))?,
		);
		write_u16(bytes, *offset, *lower);
		write_u16(bytes, *offset + 2, *upper);
		let mut cursor = *offset + 4;
		bytes[cursor..cursor + bucket.lower.len()].copy_from_slice(&bucket.lower);
		cursor += bucket.lower.len();
		bytes[cursor..cursor + bucket.upper.len()].copy_from_slice(&bucket.upper);
		cursor += bucket.upper.len();
		write_u64(bytes, cursor, bucket.frequency);
		write_u64(bytes, cursor + 8, bucket.distinct_count);
	}
	page.seal()?;
	Ok(Some(page))
}

fn decode_page(
	page: &AlignedPage,
	page_no: PageNo,
) -> FerrosGrynnResult<(PropertyHistogramKey, PropertyHistogram)> {
	let header = page.validate(page_no)?;
	if header.page_type() != PageType::PropertyHistogram || !header.flags().is_empty() {
		return Err(invalid_page(page_no, "invalid PropertyHistogram page header"));
	}
	let bytes = page.as_bytes();
	let class_id = ClassId::from_storage(read_u32(bytes, CLASS_ID))?;
	let field_id = SchemaId::from_storage(read_u32(bytes, FIELD_ID))?;
	let scope = match (bytes[SCOPE_KIND], read_u64(bytes, SCOPE_GRAPH)) {
		(0, 0) => StatisticsScope::Base,
		(1, raw) => StatisticsScope::Effective {
			graph: GraphId::new(raw)
				.ok_or_else(|| invalid_page(page_no, "effective histogram graph is zero"))?,
		},
		_ => return Err(invalid_page(page_no, "property histogram scope is invalid")),
	};
	let method = HistogramMethod::from_byte(bytes[METHOD])
		.ok_or_else(|| invalid_page(page_no, "unknown property histogram method"))?;
	let expected_precision = match method {
		HistogramMethod::Exact => EXACT_PRECISION,
		HistogramMethod::Approximate => APPROXIMATE_PRECISION,
	};
	if bytes[HLL_PRECISION] != expected_precision {
		return Err(invalid_page(page_no, "property histogram HLL precision is invalid"));
	}
	if bytes[VALUE_TYPE] == 0x27 {
		let count = usize::from(read_u16(bytes, BUCKET_COUNT));
		if count > 509 || method != HistogramMethod::Exact {
			return Err(invalid("invalid geometry sample page"));
		}
		let mut mbr_sample = Vec::with_capacity(count);
		for slot in 0..count {
			let mbr = std::array::from_fn(|axis| {
				f64::from_bits(read_u64(bytes, GEOMETRY_OFFSETS + slot * 32 + axis * 8))
			});
			crate::storage::geometry_index::validate_mbr(mbr)?;
			mbr_sample.push(mbr);
		}
		validate_zero(
			&bytes[GEOMETRY_OFFSETS + count * 32..],
			page_no,
			"geometry sample trailing bytes",
		)?;
		let histogram = PropertyHistogram {
			type_tag: 0x27,
			method,
			sample_count: read_u32(bytes, SAMPLE_COUNT),
			distinct_count: 0,
			null_fraction: f32::from_bits(read_u32(bytes, NULL_FRACTION)),
			buckets: Vec::new(),
			mbr_sample,
			mbr_population: read_u64(bytes, GEOMETRY_POPULATION),
		};
		validate_histogram(&histogram)?;
		return Ok((
			PropertyHistogramKey {
				class_id,
				field_id,
				scope,
			},
			histogram,
		));
	}
	validate_zero(&bytes[RESERVED..OFFSETS], page_no, "property histogram reserved bytes")?;
	let count = usize::from(read_u16(bytes, BUCKET_COUNT));
	if count > HISTOGRAM_BUCKETS {
		return Err(invalid_page(page_no, "property histogram exceeds bucket limit"));
	}
	let data_start = OFFSETS
		.checked_add(count * size_of::<u64>())
		.ok_or_else(|| invalid_page(page_no, "property offsets overflow"))?;
	if data_start > PAGE_SIZE {
		return Err(invalid_page(page_no, "property offsets exceed page"));
	}
	let offsets = (0..count)
		.map(|index| {
			usize::try_from(read_u64(bytes, OFFSETS + index * 8))
				.map_err(|_error| invalid_page(page_no, "property offset exceeds usize"))
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	if offsets.first().is_some_and(|offset| *offset != data_start)
		|| offsets.windows(2).any(|pair| pair[0] >= pair[1])
		|| offsets.iter().any(|offset| *offset >= PAGE_SIZE)
	{
		return Err(invalid_page(page_no, "property histogram offsets are invalid"));
	}
	let mut buckets = Vec::with_capacity(count);
	let mut used = data_start;
	for (index, offset) in offsets.iter().copied().enumerate() {
		let limit = offsets.get(index + 1).copied().unwrap_or(PAGE_SIZE);
		let header_end = offset
			.checked_add(4)
			.ok_or_else(|| invalid_page(page_no, "property bucket header overflows"))?;
		if header_end > limit {
			return Err(invalid_page(page_no, "property bucket header is truncated"));
		}
		let lower_len = usize::from(read_u16(bytes, offset));
		let upper_len = usize::from(read_u16(bytes, offset + 2));
		let upper_start = header_end
			.checked_add(lower_len)
			.ok_or_else(|| invalid_page(page_no, "property lower bound overflows"))?;
		let counters = upper_start
			.checked_add(upper_len)
			.ok_or_else(|| invalid_page(page_no, "property upper bound overflows"))?;
		let end = counters
			.checked_add(16)
			.ok_or_else(|| invalid_page(page_no, "property bucket length overflows"))?;
		if end > limit || (index + 1 < offsets.len() && end != limit) {
			return Err(invalid_page(page_no, "property bucket length is invalid"));
		}
		buckets.push(PropertyBucket {
			lower: bytes[header_end..upper_start].to_vec(),
			upper: bytes[upper_start..counters].to_vec(),
			frequency: read_u64(bytes, counters),
			distinct_count: read_u64(bytes, counters + 8),
		});
		used = end;
	}
	validate_zero(&bytes[used..], page_no, "property histogram trailing bytes")?;
	let histogram = PropertyHistogram {
		type_tag: bytes[VALUE_TYPE],
		method,
		sample_count: read_u32(bytes, SAMPLE_COUNT),
		distinct_count: read_u64(bytes, DISTINCT_COUNT),
		null_fraction: f32::from_bits(read_u32(bytes, NULL_FRACTION)),
		buckets,
		mbr_sample: Vec::new(),
		mbr_population: 0,
	};
	validate_histogram(&histogram)?;
	Ok((
		PropertyHistogramKey {
			class_id,
			field_id,
			scope,
		},
		histogram,
	))
}

fn validate_histogram(histogram: &PropertyHistogram) -> FerrosGrynnResult<()> {
	if histogram.mbr_sample.len() > 1024
		|| (histogram.type_tag != 0x27
			&& (!histogram.mbr_sample.is_empty() || histogram.mbr_population != 0))
		|| (histogram.type_tag == 0x27
			&& (!histogram.buckets.is_empty()
				|| histogram.distinct_count != 0
				|| histogram.method != HistogramMethod::Exact
				|| histogram.mbr_population > u64::from(histogram.sample_count)
				|| histogram.mbr_sample.len() as u64 > histogram.mbr_population
				|| (histogram.mbr_population == 0) != histogram.mbr_sample.is_empty()))
	{
		return Err(invalid("geometry sample metadata is invalid"));
	}
	for mbr in &histogram.mbr_sample {
		crate::storage::geometry_index::validate_mbr(*mbr)?;
	}
	let bucket_ndv = histogram.buckets.iter().try_fold(0_u64, |sum, bucket| {
		if bucket.lower > bucket.upper
			|| bucket.frequency == 0
			|| bucket.distinct_count == 0
			|| bucket.distinct_count > bucket.frequency
		{
			return Err(invalid("property histogram bucket is invalid"));
		}
		sum.checked_add(bucket.distinct_count)
			.ok_or_else(|| invalid("property histogram bucket NDV overflows"))
	})?;
	if histogram.buckets.len() > HISTOGRAM_BUCKETS
		|| !valid_type_tag(histogram.type_tag)
		|| !histogram.null_fraction.is_finite()
		|| !(0.0..=1.0).contains(&histogram.null_fraction)
		|| histogram.buckets.windows(2).any(|pair| pair[0].upper > pair[1].lower)
	{
		return Err(invalid("property histogram metadata is invalid"));
	}
	if histogram.buckets.is_empty() != (histogram.distinct_count == 0)
		|| histogram.distinct_count > bucket_ndv
	{
		return Err(invalid("property histogram NDV disagrees with buckets"));
	}
	Ok(())
}

fn valid_type_tag(value: u8) -> bool {
	matches!(value, 0x00..=0x0c | 0x10..=0x13 | 0x15 | 0x21..=0x27 | 0x31..=0x38)
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

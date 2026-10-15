use std::cell::Cell;

use super::*;
#[cfg(test)]
use crate::test_assertions::AssertOption as _;
use crate::{
	datastore::{ClassId, SchemaId},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomKind, GraphId},
	stats::{
		DegreeBucket,
		DegreeDirection,
		DegreeHistogram,
		DegreeHistogramKey,
		HistogramMethod,
		LabelStatistic,
		LabelStatisticKey,
		PropertyBucket,
		PropertyHistogram,
		PropertyHistogramKey,
		StatisticsScope,
	},
	storage::{
		graph_index::{LabelDictionary, LabelKind},
		page::{PageLsn, PageNo},
	},
	values::storage::TypeTag,
};

/// Verifies split statistics are exact and auxiliary files are lazy.
#[test]
fn split_statistics_are_exact_and_auxiliary_files_are_lazy() {
	use crate::test_assertions::AssertResult as _;

	let graph = GraphId::new(7).ok_or_else(|| invalid("graph id absent")).assert_ok();
	let class = ClassId::new(3).ok_or_else(|| invalid("class id absent")).assert_ok();
	let field = SchemaId::new(4).ok_or_else(|| invalid("field id absent")).assert_ok();
	let label: crate::values::Text = "Engineer".into();
	let label_key = LabelStatisticKey {
		kind: AtomKind::Node,
		label: label.clone(),
	};
	let mut statistics = crate::stats::GraphStatistics {
		revision: 9,
		analyzed_revision: 9,
		node_count: 2,
		total_out_degree: 1,
		total_in_degree: 1,
		maximum_out_degree: 1,
		maximum_in_degree: 1,
		..crate::stats::GraphStatistics::default()
	};
	statistics.classes.insert(class, 2);
	statistics.labels.insert(label_key.clone(), LabelStatistic {
		atom_count: 2,
		property_count: 2,
	});
	statistics.degree_histograms.insert(
		DegreeHistogramKey {
			label: Some(label_key),
			direction: DegreeDirection::Out,
		},
		DegreeHistogram {
			buckets: vec![DegreeBucket {
				minimum: 0,
				maximum: 1,
				node_count: 2,
			}],
		},
	);
	let property_key = PropertyHistogramKey {
		class_id: class,
		field_id: field,
		scope: StatisticsScope::Effective {
			graph,
		},
	};
	statistics.property_histograms.insert(property_key, PropertyHistogram {
		mbr_sample: Vec::new(),
		mbr_population: 0,
		type_tag: TypeTag::Int.byte(),
		method: HistogramMethod::Exact,
		sample_count: 2,
		distinct_count: 2,
		null_fraction: 0.0,
		buckets: vec![PropertyBucket {
			lower: vec![1],
			upper: vec![2],
			frequency: 2,
			distinct_count: 2,
		}],
	});
	let dictionary = LabelDictionary::from_labels([(LabelKind::Node, label.as_str())]).assert_ok();
	let files = encode_statistics(&statistics, &dictionary, PageLsn::new(5)).assert_ok();
	assert_eq!(files.graph.len(), 3);
	assert_eq!(files.labels.len(), 1);
	assert_eq!(files.properties.len(), 1);

	let (mut decoded, layout) = decode_statistics_root(
		&files.graph[0],
		u64_len(files.graph.len()).assert_ok(),
		u64_len(files.labels.len()).assert_ok(),
		u64_len(files.properties.len()).assert_ok(),
	)
	.assert_ok();
	assert_eq!(layout.degree_first, Some(PageNo::new(1)));
	assert_eq!(layout.degree_pages, 1);
	assert_eq!(layout.class_first, Some(PageNo::new(2)));
	assert_eq!(layout.class_pages, 1);
	assert!(decoded.classes.is_empty());
	assert!(decoded.labels.is_empty());
	assert!(decoded.degree_histograms.is_empty());
	assert!(decoded.property_histograms.is_empty());

	let reads = Cell::new(0);
	decoded.classes = decode_class_statistics(
		|page, destination| copy_page(&files.graph, page, destination, &reads),
		layout,
	)
	.assert_ok();
	assert_eq!(reads.get(), 1);
	decoded.degree_histograms = decode_degree_statistics(
		|page, destination| copy_page(&files.graph, page, destination, &reads),
		layout,
		&dictionary,
	)
	.assert_ok();
	decoded.labels = decode_label_statistics(
		|page, destination| copy_page(&files.labels, page, destination, &reads),
		layout,
		&dictionary,
	)
	.assert_ok();
	decoded.property_histograms = decode_property_statistics(
		|page, destination| copy_page(&files.properties, page, destination, &reads),
		layout,
	)
	.assert_ok();
	assert_eq!(reads.get(), 4);
	assert_eq!(decoded, statistics);
}

/// Verifies root rejects unreachable or extra split pages.
#[test]
fn root_rejects_unreachable_or_extra_split_pages() {
	use crate::test_assertions::AssertResult as _;

	let dictionary = LabelDictionary::default();
	let files =
		encode_statistics(&crate::stats::GraphStatistics::default(), &dictionary, PageLsn::ZERO)
			.assert_ok();
	assert!(decode_statistics_root(&files.graph[0], 2, 0, 0).is_err());
}

/// Verifies geometry statistics round trip bounds and continuations.
#[test]
fn geometry_statistics_round_trip_bounds_and_continuations() {
	use crate::test_assertions::AssertResult as _;

	let key = PropertyHistogramKey {
		class_id: ClassId::new(3).assert_some(),
		field_id: SchemaId::new(4).assert_some(),
		scope: StatisticsScope::Base,
	};
	for count in [0, 1, 509, 510, 1018, 1024] {
		let histogram = PropertyHistogram {
			type_tag: 0x27,
			method: HistogramMethod::Exact,
			sample_count: 2000,
			distinct_count: 0,
			null_fraction: 0.25,
			buckets: Vec::new(),
			mbr_sample: vec![[1.0, 2.0, 3.0, 4.0]; count],
			mbr_population: if count == 0 {
				0
			} else {
				1500
			},
		};
		let input = std::collections::BTreeMap::from([(key, histogram)]);
		let mut pages =
			super::property::encode_property_statistics(&input, PageLsn::ZERO).assert_ok();
		let read = |page, destination: &mut crate::storage::page::AlignedPage| {
			copy_page(&pages, page, destination, &Cell::new(0))
		};
		assert_eq!(
			super::property::decode_property_statistics_range(read, pages.len() as u32).assert_ok(),
			input
		);
		if count == 509 || count == 1018 || count == 0 {
			let mut extra = Box::new(crate::storage::page::AlignedPage::zeroed());
			extra.as_bytes_mut().copy_from_slice(pages.last().assert_some().as_bytes());
			extra.as_bytes_mut()[68..70].copy_from_slice(&0_u16.to_le_bytes());
			extra.as_bytes_mut()[70..72].copy_from_slice(&(pages.len() as u16).to_le_bytes());
			extra.as_bytes_mut()[80..].fill(0);
			extra.seal().assert_ok();
			pages.push(crate::storage::page::AlignedPage::zeroed());
			pages.last_mut().assert_some().as_bytes_mut().copy_from_slice(extra.as_bytes());
			assert!(
				super::property::decode_property_statistics_range(
					|page, destination| copy_page(&pages, page, destination, &Cell::new(0)),
					pages.len() as u32
				)
				.is_err()
			);
		}
	}
	let oversized = PropertyHistogram {
		type_tag: 0x27,
		method: HistogramMethod::Exact,
		sample_count: 2000,
		distinct_count: 0,
		null_fraction: 0.0,
		buckets: Vec::new(),
		mbr_sample: vec![[0.0; 4]; 1025],
		mbr_population: 2000,
	};
	assert!(
		super::property::encode_property_statistics(
			&std::collections::BTreeMap::from([(key, oversized)]),
			PageLsn::ZERO
		)
		.is_err()
	);
}

/// Returns copy page for this module.
fn copy_page(
	pages: &[crate::storage::page::AlignedPage],
	page: PageNo,
	destination: &mut crate::storage::page::AlignedPage,
	reads: &Cell<usize>,
) -> FerrosGrynnResult<()> {
	let index = usize::try_from(page.get()).map_err(|_error| invalid("page exceeds usize"))?;
	let source = pages.get(index).ok_or_else(|| invalid("page exceeds test file"))?;
	destination.as_bytes_mut().copy_from_slice(source.as_bytes());
	reads.set(reads.get() + 1);
	Ok(())
}

/// Returns u64 len for this module.
fn u64_len(length: usize) -> FerrosGrynnResult<u64> {
	u64::try_from(length).map_err(|_error| invalid("test length exceeds u64"))
}

/// Returns invalid for this module.
fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

use std::cell::Cell;

use super::*;
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

#[test]
fn split_statistics_are_exact_and_auxiliary_files_are_lazy() -> FerrosGrynnResult<()> {
	let graph = GraphId::new(7).ok_or_else(|| invalid("graph id absent"))?;
	let class = ClassId::new(3).ok_or_else(|| invalid("class id absent"))?;
	let field = SchemaId::new(4).ok_or_else(|| invalid("field id absent"))?;
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
	let dictionary = LabelDictionary::from_labels([(LabelKind::Node, label.as_str())])?;
	let files = encode_statistics(&statistics, &dictionary, PageLsn::new(5))?;
	assert_eq!(files.graph.len(), 3);
	assert_eq!(files.labels.len(), 1);
	assert_eq!(files.properties.len(), 1);

	let (mut decoded, layout) = decode_statistics_root(
		&files.graph[0],
		u64_len(files.graph.len())?,
		u64_len(files.labels.len())?,
		u64_len(files.properties.len())?,
	)?;
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
	)?;
	assert_eq!(reads.get(), 1);
	decoded.degree_histograms = decode_degree_statistics(
		|page, destination| copy_page(&files.graph, page, destination, &reads),
		layout,
		&dictionary,
	)?;
	decoded.labels = decode_label_statistics(
		|page, destination| copy_page(&files.labels, page, destination, &reads),
		layout,
		&dictionary,
	)?;
	decoded.property_histograms = decode_property_statistics(
		|page, destination| copy_page(&files.properties, page, destination, &reads),
		layout,
	)?;
	assert_eq!(reads.get(), 4);
	assert_eq!(decoded, statistics);
	Ok(())
}

#[test]
fn root_rejects_unreachable_or_extra_split_pages() -> FerrosGrynnResult<()> {
	let dictionary = LabelDictionary::default();
	let files =
		encode_statistics(&crate::stats::GraphStatistics::default(), &dictionary, PageLsn::ZERO)?;
	assert!(decode_statistics_root(&files.graph[0], 2, 0, 0).is_err());
	Ok(())
}

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

fn u64_len(length: usize) -> FerrosGrynnResult<u64> {
	u64::try_from(length).map_err(|_error| invalid("test length exceeds u64"))
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

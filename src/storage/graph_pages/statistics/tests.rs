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

/// Sparse class changes preserve key layout and reject malformed key order.
#[test]
fn class_population_rewrite_preserves_layout_and_validates_keys() {
	use std::collections::BTreeMap;

	use crate::test_assertions::AssertResult as _;
	let class = ClassId::new(3).assert_some();
	let next = ClassId::new(4).assert_some();
	let classes = BTreeMap::from([(class, 9), (next, 4)]);
	let mut pages = encode_class_statistics(&classes, 1, PageLsn::new(7)).assert_ok();
	let mut targets = BTreeMap::from([(class, 0)]);
	assert!(
		rewrite_class_statistics_page(&mut pages[0], PageNo::new(1), None, &mut targets)
			.assert_ok()
	);
	assert!(targets.is_empty());
	let decoded = decode_class_statistics_range(
		|number, output| {
			assert_eq!(number, PageNo::new(1));
			output.as_bytes_mut().copy_from_slice(pages[0].as_bytes());
			Ok(())
		},
		Some(PageNo::new(1)),
		1,
	)
	.assert_ok();
	assert_eq!(decoded, BTreeMap::from([(class, 0), (next, 4)]));
	let mut targets = BTreeMap::from([(class, 0)]);
	assert!(
		!rewrite_class_statistics_page(&mut pages[0], PageNo::new(1), None, &mut targets)
			.assert_ok()
	);
	// Reverse valid class keys while retaining a correct checksum.
	pages[0].as_bytes_mut()[48..52].copy_from_slice(&next.get().to_le_bytes());
	pages[0].as_bytes_mut()[64..68].copy_from_slice(&class.get().to_le_bytes());
	pages[0].seal().assert_ok();
	let saved = pages[0].as_bytes().to_vec();
	let mut targets = BTreeMap::from([(class, 2)]);
	assert!(
		rewrite_class_statistics_page(&mut pages[0], PageNo::new(1), None, &mut targets).is_err()
	);
	assert_eq!(targets, BTreeMap::from([(class, 2)]));
	assert_eq!(pages[0].as_bytes().as_slice(), saved);
}

/// Schema bounds reject excessive pages before reads and entries before growth.
#[test]
fn class_decode_schema_bound_precedes_reads_and_map_growth() {
	use std::collections::BTreeMap;

	use crate::test_assertions::AssertResult as _;
	let classes =
		BTreeMap::from([(ClassId::new(1).assert_some(), 1), (ClassId::new(2).assert_some(), 0)]);
	let pages = encode_class_statistics(&classes, 1, PageLsn::ZERO).assert_ok();
	let layout = StatisticsLayout::from_counts(0, 1, 0, 0).assert_ok();
	let reads = Cell::new(0);
	let mut read = |number, output: &mut crate::storage::page::AlignedPage| {
		assert_eq!(number, PageNo::new(1));
		reads.set(reads.get() + 1);
		output.as_bytes_mut().copy_from_slice(pages[0].as_bytes());
		Ok(())
	};
	assert!(decode_class_statistics_bounded(&mut read, layout, 0).is_err());
	assert_eq!(reads.get(), 0);
	assert!(decode_class_statistics_bounded(&mut read, layout, 1).is_err());
	assert_eq!(reads.get(), 1);
	assert_eq!(decode_class_statistics_bounded(&mut read, layout, 2).assert_ok(), classes);
}

/// Sparse writes validate all entries before consuming any target.
#[test]
fn label_counter_rewrite_is_sparse_and_rejects_invalid_sources() {
	use crate::test_assertions::AssertResult as _;

	let dictionary = LabelDictionary::from_labels([
		(LabelKind::Node, "a"),
		(LabelKind::Node, "b"),
		(LabelKind::Edge, "a"),
	])
	.assert_ok();
	let mut statistics = crate::stats::GraphStatistics::default();
	for (kind, label) in [(AtomKind::Node, "a"), (AtomKind::Node, "b"), (AtomKind::Edge, "a")] {
		statistics.labels.insert(
			LabelStatisticKey {
				kind,
				label: label.into(),
			},
			LabelStatistic {
				atom_count: 1,
				property_count: 2,
			},
		);
	}
	let mut pages =
		label::encode_label_statistics(&statistics, &dictionary, PageLsn::new(7)).assert_ok();
	let key = (LabelKind::Node, 1);
	let old = LabelStatistic {
		atom_count: 1,
		property_count: 2,
	};
	let new = LabelStatistic {
		atom_count: 1,
		property_count: 1,
	};
	let changes = LabelStatisticsChanges::from([(key, (old.clone(), new.clone()))]);
	let original = pages[0].as_bytes().to_vec();
	for (offset, value, reseal) in [
		(48, 2, true),
		(52, 2, true),
		(76, 0, true),
		(73, 1, true),
		(44, 1, true),
		(120, 1, true),
		(32, 0, true),
		(80, 0, true),
		(88, 3, true),
		(40, 255, true),
		(64, 3, false),
	] {
		pages[0].as_bytes_mut().copy_from_slice(&original);
		pages[0].as_bytes_mut()[offset] = value;
		if reseal {
			pages[0].seal().assert_ok();
		}
		let malformed = pages[0].as_bytes().to_vec();
		let mut pending = changes.clone();
		assert!(
			rewrite_label_statistics_page(
				&mut pages[0],
				PageNo::new(0),
				None,
				[2, 1],
				&mut pending
			)
			.is_err()
		);
		assert_eq!(pending, changes);
		assert_eq!(pages[0].as_bytes().as_slice(), malformed);
	}
	pages[0].as_bytes_mut().copy_from_slice(&original);
	let mut invalid = new.clone();
	invalid.atom_count = 0;
	let mut pending = LabelStatisticsChanges::from([(key, (old, invalid))]);
	assert!(
		rewrite_label_statistics_page(&mut pages[0], PageNo::new(0), None, [2, 1], &mut pending)
			.is_err()
	);
	assert_eq!(pages[0].as_bytes().as_slice(), original);
	let mut pending = changes;
	assert!(
		rewrite_label_statistics_page(&mut pages[0], PageNo::new(0), None, [2, 1], &mut pending)
			.assert_ok()
	);
	assert!(pending.is_empty());
	let mut pending = LabelStatisticsChanges::from([(key, (new.clone(), new))]);
	assert!(
		!rewrite_label_statistics_page(&mut pages[0], PageNo::new(0), None, [2, 1], &mut pending)
			.assert_ok()
	);
	assert!(pending.is_empty());
	// Apart from the checksum and the selected counter, the layout is
	// identical.
	let bytes = pages[0].as_bytes();
	assert_eq!(&bytes[32..88], &original[32..88]);
	assert_eq!(&bytes[96..], &original[96..]);
}

/// Missing dictionary labels must not shift the dense IDs used by sparse
/// writes.
#[test]
fn label_statistics_reject_incomplete_namespaces() {
	use crate::test_assertions::AssertResult as _;

	let dictionary = LabelDictionary::from_labels([
		(LabelKind::Node, "a"),
		(LabelKind::Node, "b"),
		(LabelKind::Node, "c"),
	])
	.assert_ok();
	let mut statistics = crate::stats::GraphStatistics::default();
	for label in ["b", "c"] {
		statistics.labels.insert(
			LabelStatisticKey {
				kind: AtomKind::Node,
				label: label.into(),
			},
			LabelStatistic {
				atom_count: 1,
				property_count: 0,
			},
		);
	}
	let pages = label::encode_label_statistics(&statistics, &dictionary, PageLsn::ZERO).assert_ok();
	let layout = StatisticsLayout::from_counts(0, 0, pages.len(), 0).assert_ok();
	assert!(
		decode_label_statistics(
			|page, destination| copy_page(&pages, page, destination, &Cell::new(0)),
			layout,
			&dictionary
		)
		.is_err()
	);
}

/// A late label target leaves earlier pages unstaged and preserves their bytes.
#[test]
fn label_counter_rewrite_only_changes_the_matching_page() {
	use crate::test_assertions::AssertResult as _;

	let names = (0..681).map(|id| format!("label-{id:04}")).collect::<Vec<_>>();
	let dictionary =
		LabelDictionary::from_labels(names.iter().map(|name| (LabelKind::Node, name.as_str())))
			.assert_ok();
	let mut statistics = crate::stats::GraphStatistics::default();
	let old = LabelStatistic {
		atom_count: 1,
		property_count: 2,
	};
	let new = LabelStatistic {
		atom_count: 1,
		property_count: 1,
	};
	for name in &names {
		statistics.labels.insert(
			LabelStatisticKey {
				kind: AtomKind::Node,
				label: name.as_str().into(),
			},
			old.clone(),
		);
	}
	let mut pages =
		label::encode_label_statistics(&statistics, &dictionary, PageLsn::ZERO).assert_ok();
	assert_eq!(pages.len(), 2);
	let saved = pages[0].as_bytes().to_vec();
	let mut pending = LabelStatisticsChanges::from([((LabelKind::Node, 680), (old, new.clone()))]);
	assert!(
		!rewrite_label_statistics_page(
			&mut pages[0],
			PageNo::new(0),
			Some(PageNo::new(1)),
			[681, 0],
			&mut pending
		)
		.assert_ok()
	);
	assert_eq!(pages[0].as_bytes().as_slice(), saved);
	assert_eq!(pending.len(), 1);
	assert!(
		rewrite_label_statistics_page(&mut pages[1], PageNo::new(1), None, [681, 0], &mut pending)
			.assert_ok()
	);
	assert!(pending.is_empty());
	let layout = StatisticsLayout::from_counts(0, 0, pages.len(), 0).assert_ok();
	let decoded = decode_label_statistics(
		|page, destination| copy_page(&pages, page, destination, &Cell::new(0)),
		layout,
		&dictionary,
	)
	.assert_ok();
	statistics.labels.insert(
		LabelStatisticKey {
			kind: AtomKind::Node,
			label: "label-0680".into(),
		},
		new,
	);
	assert_eq!(decoded, statistics.labels);
}

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
	let graph_layout =
		GraphStatisticsLayout::measure(&statistics, files.labels.len(), files.properties.len())
			.assert_ok();
	let graph_only =
		encode_graph_statistics(&statistics, &dictionary, &graph_layout, PageLsn::new(5))
			.assert_ok();
	assert_eq!(graph_layout.output_pages, graph_only.len());
	for (selected, full) in graph_only.iter().zip(&files.graph) {
		assert_eq!(selected.as_bytes(), full.as_bytes());
	}
	assert_eq!(files.graph.len(), 3);
	assert_eq!(files.labels.len(), 1);
	assert_eq!(files.properties.len(), 1);
	let measured =
		GraphLabelStatisticsLayout::measure(&statistics, files.properties.len()).assert_ok();
	let selected =
		encode_graph_label_statistics(&statistics, &dictionary, &measured, PageLsn::new(5))
			.assert_ok();
	assert_eq!(selected.graph.len(), files.graph.len());
	assert_eq!(selected.labels.len(), files.labels.len());
	for (selected, full) in
		selected.graph.iter().chain(&selected.labels).zip(files.graph.iter().chain(&files.labels))
	{
		assert_eq!(selected.as_bytes(), full.as_bytes());
	}

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

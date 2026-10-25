//! Directory traversal follows validated links rather than arena-slot
//! arithmetic.

use super::{support::*, *};
use crate::{
	storage::{
		graph_pages::{
			build_graph_image,
			decode_subgraph_directory,
			encode_subgraph_directory,
			open_graph_image,
		},
		page::PageLsn,
	},
	values::storage::EncodeContext,
};

/// A valid 0->2->1 chain keeps its layout and patches B on physical page one.
#[test]
fn bounded_child_directory_preserves_noncanonical_page_chain() {
	let fixture = fixture(IoMode::Sync, 1021);
	let store = local(&fixture.store);
	let parent = fixture.parents[0].id();
	let parent_files = files(&fixture, parent);
	let (before, after, delta) = roots(&fixture, &[(1, 0)]);
	let root = fixture.store.inner.current_root();
	let mut context = EncodeContext::new(root.dictionary.as_ref().clone());
	let mut image = build_graph_image(&root, parent, &mut context, PageLsn::ZERO).assert_ok();
	assert_eq!(image.subgraphs.len(), 3);
	let entries = image
		.subgraphs
		.iter()
		.enumerate()
		.map(|(number, page)| {
			decode_subgraph_directory(page, PageNo::new(u64::try_from(number).assert_ok()))
				.assert_ok()
				.1
		})
		.collect::<Vec<_>>();
	let physical = [(0, Some(2)), (2, None), (1, Some(1))];
	let mut source = Vec::new();
	for (index, (entry_index, next)) in physical.into_iter().enumerate() {
		let number = PageNo::new(u64::try_from(index).assert_ok());
		let next = next.map(PageNo::new);
		let page = encode_subgraph_directory(number, PageLsn::ZERO, next, &entries[entry_index])
			.assert_ok();
		let key = PageKey::new(parent_files.space(), parent_files.subgraphs, number);
		let mut tx = store.wal.begin_tx().assert_ok();
		tx.stage_page(key, &page).assert_ok();
		store.wal.commit_tx(tx).assert_ok();
		source.push((next, entries[entry_index].clone()));
		image.subgraphs[index] = page;
	}
	// The existing complete-image reader independently accepts this chain
	// shape.
	let opened = open_graph_image(image, &root, &context.decoder()).assert_ok();
	assert_eq!(
		opened.occurrences().count(),
		root.graph(parent).assert_some().occurrences().count()
	);
	drop(root);
	let boundary = store.wal.snapshot().visible_lsn();
	// Reuse pre-injection roots across the deliberate logical/WAL clock
	// mismatch.
	let plan = prepare(store, &before, &after, &delta).assert_ok();
	let directories = plan
		.pages
		.iter()
		.filter(|(key, _)| key.file_id().kind() == Some(DataFileKind::Subgraphs))
		.collect::<Vec<_>>();
	assert_eq!(directories.len(), 1);
	let (key, page) = directories[0];
	assert_eq!(*key, PageKey::new(parent_files.space(), parent_files.subgraphs, PageNo::new(1)));
	let (next, changed) = decode_subgraph_directory(page, key.page_no()).assert_ok();
	assert_eq!(next, None);
	assert_eq!(changed.len(), 1);
	let mut expected = source[1].1[0];
	expected.node_capacity += 1;
	expected.node_count += 1;
	assert_eq!(changed, [expected]);
	assert_eq!(directory(&fixture, parent), source);
	assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
}

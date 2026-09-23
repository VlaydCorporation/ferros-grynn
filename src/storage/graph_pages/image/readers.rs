//! Bounded readers for graph-image page vectors and overflow chains.

use std::collections::BTreeSet;

use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::{
		graph_pages::{
			EndpointSide,
			QualifiedDiskAtomRef,
			SubgraphEntry,
			decode_adjacency_overflow,
			decode_cross_level_overflow,
			decode_subgraph_directory,
		},
		page::{AlignedPage, PageNo},
	},
	values::storage::DiskAtomRef,
};

pub(super) fn copy_page(
	pages: &[AlignedPage],
	page_no: PageNo,
	output: &mut AlignedPage,
	family: &str,
) -> FerrosGrynnResult<()> {
	let index = usize::try_from(page_no.get())
		.map_err(|_error| invalid(format!("{family} page number exceeds usize")))?;
	let source = pages
		.get(index)
		.ok_or_else(|| invalid(format!("{family} page reference exceeds its file")))?;
	output.as_bytes_mut().copy_from_slice(source.as_bytes());
	Ok(())
}

pub(super) fn local_chain(
	pages: &[AlignedPage],
	first: Option<PageNo>,
) -> FerrosGrynnResult<Vec<DiskAtomRef>> {
	let mut result = Vec::new();
	let mut current = first;
	let mut visited = BTreeSet::new();
	while let Some(page_no) = current {
		if !visited.insert(page_no) {
			return Err(invalid("adjacency overflow chain contains a cycle"));
		}
		let page = page_at(pages, page_no, "adjacency")?;
		let (next, entries) = decode_adjacency_overflow(page, page_no)?;
		result.extend(entries);
		current = next;
	}
	Ok(result)
}

pub(super) fn qualified_chain(
	pages: &[AlignedPage],
	first: Option<PageNo>,
	side: EndpointSide,
) -> FerrosGrynnResult<Vec<QualifiedDiskAtomRef>> {
	let mut result = Vec::new();
	let mut current = first;
	let mut visited = BTreeSet::new();
	while let Some(page_no) = current {
		if !visited.insert(page_no) {
			return Err(invalid("cross-level overflow chain contains a cycle"));
		}
		let page = page_at(pages, page_no, "cross-level adjacency")?;
		let (next, entries) = decode_cross_level_overflow(page, page_no)?;
		if entries.iter().any(|entry| entry.side != side) {
			return Err(invalid("cross-level overflow chain mixes endpoint sides"));
		}
		result.extend(entries);
		current = next;
	}
	Ok(result)
}

pub(super) fn subgraph_entries(pages: &[AlignedPage]) -> FerrosGrynnResult<Vec<SubgraphEntry>> {
	if pages.is_empty() {
		return Err(invalid("subgraph directory has no root page"));
	}
	let mut result = Vec::new();
	let mut current = Some(PageNo::new(0));
	let mut visited = BTreeSet::new();
	while let Some(page_no) = current {
		if !visited.insert(page_no) {
			return Err(invalid("subgraph directory contains a cycle"));
		}
		let page = page_at(pages, page_no, "subgraph directory")?;
		let (next, entries) = decode_subgraph_directory(page, page_no)?;
		result.extend(entries);
		current = next;
	}
	if visited.len() != pages.len() {
		return Err(invalid("subgraph file contains unreachable pages"));
	}
	Ok(result)
}

fn page_at<'pages>(
	pages: &'pages [AlignedPage],
	page_no: PageNo,
	family: &str,
) -> FerrosGrynnResult<&'pages AlignedPage> {
	let index = usize::try_from(page_no.get())
		.map_err(|_error| invalid(format!("{family} page number exceeds usize")))?;
	pages.get(index).ok_or_else(|| invalid(format!("{family} page reference exceeds its file")))
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

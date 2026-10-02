//! Snapshot-exact engine-owned qualified edge endpoint postings.

use std::{collections::BTreeMap, ops::Bound, path::Path};

use crate::{
	datastore::{DatabaseRoot, SystemIndex},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomKind, EdgeEndpoint, GraphId},
	storage::{
		graph_index::{
			ExternalIndexSorter,
			SortedIndexRecords,
			StreamedIndex,
			external_sort_run_budget,
			stream_unique_index_with_layout,
			visit_unique_range,
		},
		graph_pages::GraphArenaBindings,
		page::{AlignedPage, PAGE_SIZE, PageFlags, PageLsn, PageNo, PageType},
	},
	values::storage::DiskAtomRef,
};

/// Qualified endpoint plus connector and side occupy one canonical unique leaf
/// key.
const KEY_BYTES: usize = 24;
/// A query admits bounded postings before retaining any connector result.
const MAX_POSTINGS: usize = 262_144;

/// Checksummed ownership and exact tree extent of one synchronous capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EndpointHeader {
	/// Owning graph, including its independent occurrence namespace.
	pub(super) graph: GraphId,
	/// Shared schema identity and definition version.
	pub(super) capability: SystemIndex,
	/// Snapshot revision published with topology.
	pub(super) revision: u64,
	/// Root of the concrete endpoint B+tree.
	pub(super) root: PageNo,
	/// Complete file extent including the header at page zero.
	pub(super) page_count: u64,
	/// Number of distinct connector-side postings.
	pub(super) posting_count: u64,
}

/// Sorts borrowed runtime postings without materializing another row
/// collection.
pub(super) fn prepare(
	database: &DatabaseRoot,
	graph: GraphId,
	arena: &GraphArenaBindings,
	scratch: &Path,
) -> FerrosGrynnResult<(SortedIndexRecords, u64)> {
	let graph = database.graph(graph).ok_or_else(|| invalid("endpoint graph is absent"))?;
	let budget = external_sort_run_budget();
	let scope_bytes = arena
		.scope_by_slot
		.len()
		.checked_mul(96)
		.ok_or_else(|| invalid("endpoint scope metadata overflows"))?;
	if scope_bytes > budget / 4 {
		return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			"edge endpoint scope metadata",
			(budget / 4) as u64,
		));
	}
	let scopes = arena
		.scope_by_slot
		.iter()
		.map(|(slot, scope)| (scope.as_ref(), *slot))
		.collect::<BTreeMap<_, _>>();
	let mut sorter = ExternalIndexSorter::with_budget(scratch, budget / 4);
	let mut count = 0_u64;
	for (endpoint, connector, side) in graph.edge_endpoint_entries() {
		let (scope, atom) = match endpoint {
			EdgeEndpoint::Local(atom) => (0, *atom),
			EdgeEndpoint::Nested(qualified) => (
				*scopes
					.get(qualified.scope())
					.ok_or_else(|| invalid("qualified endpoint scope has no arena slot"))?,
				qualified.atom(),
			),
		};
		let endpoint = DiskAtomRef::from_parts(AtomKind::Edge, atom.slot())?;
		let connector = DiskAtomRef::from_parts(AtomKind::Edge, connector.slot())?;
		let mut key = prefix(scope, endpoint).to_vec();
		key.extend_from_slice(&connector.raw().to_be_bytes());
		key.push(u8::from(side));
		key.extend_from_slice(&[0; 3]);
		sorter.push(key, connector)?;
		count = count.checked_add(1).ok_or_else(|| invalid("endpoint posting count overflows"))?;
	}
	Ok((sorter.finish()?, count))
}

/// Writes the shared sorted tree and its header to an unreachable generation.
pub(super) fn stream(
	records: &SortedIndexRecords,
	mut header: EndpointHeader,
	scratch: &Path,
	mut write: impl FnMut(PageNo, &AlignedPage) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<StreamedIndex> {
	let mut page = Box::new(AlignedPage::zeroed());
	encode_header(&mut page, header)?;
	write(PageNo::new(0), &page)?;
	let tree = stream_unique_index_with_layout::<DiskAtomRef>(
		records,
		scratch,
		PageLsn::ZERO,
		PageType::IndexEdgeEndpointEntry,
		PageNo::new(1),
		external_sort_run_budget() / 4,
		&mut write,
	)?;
	header.root = tree.root;
	header.page_count = tree.page_count;
	encode_header(&mut page, header)?;
	write(PageNo::new(0), &page)?;
	Ok(StreamedIndex {
		root: PageNo::new(0),
		page_count: tree.page_count,
	})
}

/// Allocates final aligned slots before filling a buffered generation used by
/// codec tests.
pub(super) fn buffered(
	records: &SortedIndexRecords,
	header: EndpointHeader,
	scratch: &Path,
) -> FerrosGrynnResult<Vec<AlignedPage>> {
	let layout = stream(records, header, scratch, |_number, _page| Ok(()))?;
	let count = usize::try_from(layout.page_count)
		.map_err(|_error| invalid("endpoint page count exceeds this platform"))?;
	let bytes =
		count.checked_mul(PAGE_SIZE).ok_or_else(|| invalid("endpoint image bytes overflow"))?;
	if bytes > 128 * 1024 * 1024 {
		return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			"buffered endpoint pages",
			128 * 1024 * 1024,
		));
	}
	let mut pages = Vec::new();
	pages
		.try_reserve_exact(count)
		.map_err(|cause| invalid("endpoint page allocation failed").with_technical_source(cause))?;
	pages.resize_with(count, AlignedPage::zeroed);
	stream(records, header, scratch, |number, page| {
		let target = pages
			.get_mut(number.get() as usize)
			.ok_or_else(|| invalid("endpoint sizing and writing disagree"))?;
		target.as_bytes_mut().copy_from_slice(page.as_bytes());
		Ok(())
	})?;
	Ok(pages)
}

/// Decodes the catalog-independent physical extent and rejects reserved bytes.
pub(super) fn decode_header(
	page: &AlignedPage,
	graph: GraphId,
	capability: SystemIndex,
	revision: u64,
	page_count: u64,
) -> FerrosGrynnResult<EndpointHeader> {
	let common = page.validate(PageNo::new(0))?;
	let bytes = page.as_bytes();
	let read_u64 = |offset| {
		let mut value = [0; 8];
		value.copy_from_slice(&bytes[offset..offset + 8]);
		u64::from_le_bytes(value)
	};
	let read_u32 = |offset| {
		let mut value = [0; 4];
		value.copy_from_slice(&bytes[offset..offset + 4]);
		u32::from_le_bytes(value)
	};
	let root = PageNo::new(read_u64(56));
	if common.page_type() != PageType::IndexEdgeEndpointHeader
		|| read_u32(32) != capability.id().get()
		|| read_u32(36) != capability.version().get()
		|| read_u64(40) != graph.get()
		|| read_u64(48) != revision
		|| read_u64(64) != page_count
		|| page_count < 2
		|| root.get() == 0
		|| root.get() >= page_count
		|| read_u64(72) > (page_count - 1).saturating_mul((PAGE_SIZE / 32) as u64)
		|| bytes[80..PAGE_SIZE].iter().any(|byte| *byte != 0)
	{
		return Err(invalid("edge endpoint header disagrees with its exact snapshot"));
	}
	Ok(EndpointHeader {
		graph,
		capability,
		revision,
		root,
		page_count,
		posting_count: read_u64(72),
	})
}

/// Visits only the matching qualified key prefix with bounded owned postings.
pub(super) fn lookup(
	mut read: impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	header: EndpointHeader,
	scope: u32,
	endpoint: DiskAtomRef,
) -> FerrosGrynnResult<Vec<(DiskAtomRef, bool)>> {
	if endpoint.kind() != AtomKind::Edge {
		return Err(invalid("endpoint lookup requires an edge"));
	}
	let lower = prefix(scope, endpoint);
	let mut upper = lower;
	let mut has_upper = false;
	for byte in upper.iter_mut().rev() {
		if *byte == u8::MAX {
			*byte = 0;
		} else {
			*byte += 1;
			has_upper = true;
			break;
		}
	}
	let mut postings = Vec::new();
	let mut page_reads = 0_usize;
	let mut bounded_read = |number: PageNo, page: &mut AlignedPage| {
		if number.get() == 0 || number.get() >= header.page_count {
			return Err(invalid("endpoint tree page exceeds its exact extent"));
		}
		if page_reads == 4096 {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"endpoint tree reads",
				4096,
			));
		}
		page_reads += 1;
		read(number, page)
	};
	visit_unique_range(
		&mut bounded_read,
		header.root,
		PageType::IndexEdgeEndpointEntry,
		&Bound::Included(lower.as_slice()),
		&if has_upper {
			Bound::Excluded(upper.as_slice())
		} else {
			Bound::Unbounded
		},
		&mut |key, connector: DiskAtomRef| {
			if key.len() != KEY_BYTES
				|| key[..12] != lower
				|| key[21..].iter().any(|byte| *byte != 0)
				|| key[20] > 1
				|| connector.kind() != AtomKind::Edge
				|| key[12..20] != connector.raw().to_be_bytes()
			{
				return Err(invalid("endpoint leaf posting is not canonical"));
			}
			if postings.len() == MAX_POSTINGS {
				return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
					"edge endpoint postings",
					MAX_POSTINGS as u64,
				));
			}
			postings.push((connector, key[20] == 1));
			Ok(())
		},
	)?;
	if postings.len() as u64 > header.posting_count {
		return Err(invalid("endpoint lookup exceeds its declared posting count"));
	}
	Ok(postings)
}

/// Forms an order-preserving exact occurrence key, never a bare base atom
/// identity.
fn prefix(scope: u32, endpoint: DiskAtomRef) -> [u8; 12] {
	let mut bytes = [0; 12];
	bytes[..4].copy_from_slice(&scope.to_be_bytes());
	bytes[4..].copy_from_slice(&endpoint.raw().to_be_bytes());
	bytes
}

/// Fills one final page without returning an initialized aligned value by
/// value.
fn encode_header(page: &mut AlignedPage, header: EndpointHeader) -> FerrosGrynnResult<()> {
	page.reset(
		PageType::IndexEdgeEndpointHeader,
		PageFlags::empty(),
		PageLsn::ZERO,
		PageNo::new(0),
	);
	let bytes = page.as_bytes_mut();
	bytes[32..36].copy_from_slice(&header.capability.id().get().to_le_bytes());
	bytes[36..40].copy_from_slice(&header.capability.version().get().to_le_bytes());
	for (offset, value) in [
		(40, header.graph.get()),
		(48, header.revision),
		(56, header.root.get()),
		(64, header.page_count),
		(72, header.posting_count),
	] {
		bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
	}
	page.seal()
}

/// Reports corruption through the shared typed error boundary.
fn invalid(reason: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::datastore::{IndexId, SystemIndexKind};

	#[test]
	fn endpoint_pages_distinguish_scopes_and_check_metadata_and_reserved_keys()
	-> FerrosGrynnResult<()> {
		let graph = GraphId::new(1).ok_or_else(|| invalid("test graph id"))?;
		let capability = SystemIndex::new(
			IndexId::new(9).ok_or_else(|| invalid("test index id"))?,
			SystemIndexKind::EdgeEndpoint,
		);
		let endpoint = DiskAtomRef::from_parts(AtomKind::Edge, 17)?;
		let scratch = std::env::temp_dir();
		let mut sorter = ExternalIndexSorter::with_budget(&scratch, 8192);
		for index in 0..5000_u64 {
			let connector = DiskAtomRef::from_parts(AtomKind::Edge, index)?;
			let mut key = prefix((index % 2) as u32, endpoint).to_vec();
			key.extend_from_slice(&connector.raw().to_be_bytes());
			key.extend_from_slice(&[u8::from(index % 3 == 0), 0, 0, 0]);
			sorter.push(key, connector)?;
		}
		let records = sorter.finish()?;
		let seed = EndpointHeader {
			graph,
			capability,
			revision: 7,
			root: PageNo::NULL,
			page_count: 0,
			posting_count: 5000,
		};
		let mut pages = buffered(&records, seed, &scratch)?;
		let header = decode_header(&pages[0], graph, capability, 7, pages.len() as u64)?;
		assert!(pages.len() > 3);
		assert_eq!(&pages[0].as_bytes()[32..40], &[9, 0, 0, 0, 1, 0, 0, 0]);
		for scope in [0, 1, 2] {
			let actual = lookup(
				|number, output| {
					output.as_bytes_mut().copy_from_slice(pages[number.get() as usize].as_bytes());
					Ok(())
				},
				header,
				scope,
				endpoint,
			)?;
			assert_eq!(
				actual.len(),
				if scope < 2 {
					2500
				} else {
					0
				}
			);
			assert!(
				actual.iter().all(|(connector, side)| connector.slot() % 2 == u64::from(scope)
					&& *side == (connector.slot() % 3 == 0))
			);
		}
		assert!(decode_header(&pages[0], graph, capability, 8, pages.len() as u64).is_err());
		pages[0].as_bytes_mut()[80] = 1;
		pages[0].seal()?;
		assert!(decode_header(&pages[0], graph, capability, 7, pages.len() as u64).is_err());
		let mut sorter = ExternalIndexSorter::new(&scratch);
		let mut malformed = prefix(0, endpoint).to_vec();
		malformed.extend_from_slice(&endpoint.raw().to_be_bytes());
		malformed.extend_from_slice(&[0, 1, 0, 0]);
		sorter.push(malformed, endpoint)?;
		let records = sorter.finish()?;
		let pages = buffered(
			&records,
			EndpointHeader {
				posting_count: 1,
				..seed
			},
			&scratch,
		)?;
		let header = decode_header(&pages[0], graph, capability, 7, pages.len() as u64)?;
		assert!(
			lookup(
				|number, output| {
					output.as_bytes_mut().copy_from_slice(pages[number.get() as usize].as_bytes());
					Ok(())
				},
				header,
				0,
				endpoint
			)
			.is_err()
		);
		Ok(())
	}
}

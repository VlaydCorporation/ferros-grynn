//! Lazy decoding of immutable scalar and count index side-files.

use std::{
	collections::{BTreeMap, BTreeSet, VecDeque},
	sync::Arc,
};

use parking_lot::Mutex;

use super::{
	LocalStore,
	physical_traversal::{LocalQuerySnapshot, read_snapshot_pages},
};
use crate::{
	datastore::{
		ClassId,
		ClassIndexKind,
		ClassIndexOptions,
		DataSpaceId,
		IndexId,
		IndexScope,
		SchemaVersion,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::AtomKind,
	storage::{
		catalog::{DatabaseCatalog, PhysicalIndexBinding},
		file_registry::{FileRegistry, RegisteredDataFile},
		fulltext_index::{DecodedFulltextIndex, decode_fulltext_index},
		fulltext_index_delta::{DecodedFulltextDelta, decode_fulltext_delta},
		graph_index::{CountIndexScope, ScalarIndexMode, decode_count_index, scan_scalar_index},
		hybrid_engine::MAX_READ_SCOPE_OPERATIONS,
		page::{AlignedPage, PageHeader, PageNo},
		vector_index::{
			DecodedVectorDelta,
			DecodedVectorIndex,
			PagedVectorNode,
			STREAM_PAYLOAD_BYTES,
			VECTOR_NODE_HEADER_BYTES,
			VectorNodeLocation,
			VectorNodeRecordLayout,
			VectorSearchHit,
			decode_validated_vector_stream_page,
			decode_vector_delta,
			decode_vector_directory_entry,
			decode_vector_header,
			decode_vector_index,
			decode_vector_node_parts,
			decode_vector_node_record_layout,
			search_paged_diskann,
			validate_vector_file_pages,
			vector_directory_address,
		},
	},
	values::{Vector, storage::DiskAtomRef},
};

const DISKANN_PAGE_WINDOW: usize = 32;
const DISKANN_PAGE_CACHE_PAGES: usize = MAX_READ_SCOPE_OPERATIONS;

/// Generation-free address returned by one immutable physical index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PhysicalIndexAtom {
	pub(crate) kind: AtomKind,
	pub(crate) slot: u64,
}

/// One scalar key and its canonically ordered postings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PhysicalScalarGroup {
	pub(crate) key: Vec<u8>,
	pub(crate) atoms: Vec<PhysicalIndexAtom>,
}

/// Decoded immutable payload selected by the catalog index kind.
#[derive(Debug)]
pub(crate) enum PhysicalIndexEntries {
	Scalar(Vec<PhysicalScalarGroup>),
	Count(u64),
	Fulltext(DecodedFulltextIndex),
	Vector(DecodedVectorIndex),
}

/// Exact replacement/tombstone generation attached to an immutable base.
#[derive(Debug)]
pub(crate) enum PhysicalIndexDelta {
	Fulltext(DecodedFulltextDelta),
	Vector(DecodedVectorDelta),
}

/// Exact logical metadata and lazily decoded payload of one durable index.
#[derive(Debug)]
pub(crate) struct PhysicalIndexRead {
	pub(crate) class_id: ClassId,
	pub(crate) kind: ClassIndexKind,
	pub(crate) scope: IndexScope,
	pub(crate) entries: PhysicalIndexEntries,
	pub(crate) delta: Option<PhysicalIndexDelta>,
}

/// One current-generation file with a coalesced first decode.
pub(super) struct PhysicalIndexFile {
	index_id: IndexId,
	version: SchemaVersion,
	class_id: ClassId,
	kind: ClassIndexKind,
	scope: IndexScope,
	options: ClassIndexOptions,
	binding: PhysicalIndexBinding,
	file: Arc<RegisteredDataFile>,
	delta_binding: Option<PhysicalIndexBinding>,
	delta_file: Option<Arc<RegisteredDataFile>>,
	decoded: Mutex<Option<Arc<PhysicalIndexRead>>>,
}

impl LocalStore {
	/// Returns whether an exact immutable side-file belongs to this snapshot.
	pub(crate) fn has_physical_index(&self, snapshot: &LocalQuerySnapshot, index: IndexId) -> bool {
		snapshot.0.indexes.contains_key(&index)
	}

	/// Decodes one immutable side-file at first demand and shares it for every
	/// later statement retaining the same physical generation.
	pub(crate) fn physical_index(
		&self,
		snapshot: &LocalQuerySnapshot,
		index: IndexId,
	) -> FerrosGrynnResult<Option<Arc<PhysicalIndexRead>>> {
		let Some(file) = snapshot.0.indexes.get(&index) else {
			return Ok(None);
		};
		file.read(self, snapshot).map(Some)
	}

	/// Searches DiskANN through bounded snapshot-exact page windows without
	/// decoding or retaining the complete base generation.
	pub(crate) fn search_diskann_index(
		&self,
		snapshot: &LocalQuerySnapshot,
		index: IndexId,
		query: &Vector,
		k: usize,
		breadth: usize,
	) -> FerrosGrynnResult<Option<Vec<VectorSearchHit>>> {
		let Some(file) = snapshot.0.indexes.get(&index) else {
			return Ok(None);
		};
		if file.kind != ClassIndexKind::DiskAnn {
			return Ok(None);
		}
		file.search_diskann(self, snapshot, query, k, breadth).map(Some)
	}
}

impl PhysicalIndexFile {
	fn search_diskann(
		&self,
		store: &LocalStore,
		snapshot: &LocalQuerySnapshot,
		query: &Vector,
		k: usize,
		breadth: usize,
	) -> FerrosGrynnResult<Vec<VectorSearchHit>> {
		let space = match self.scope {
			IndexScope::Base => DataSpaceId::Database,
			IndexScope::Effective {
				graph,
			} => DataSpaceId::Graph(graph),
		};
		let mut provider = FerrosDiskAnnProvider {
			store,
			snapshot: &snapshot.0,
			space,
			binding: self.binding,
			file: &self.file,
			pages: VecDeque::with_capacity(DISKANN_PAGE_CACHE_PAGES),
		};
		let header_page = provider.read_page(PageNo::new(0))?;
		let header = decode_vector_header(&header_page.page)?;
		validate_vector_file_pages(&header, self.file.page_count())?;
		if header.spec.index != self.index_id
			|| header.spec.index_version != self.version
			|| header.spec.class != self.class_id
			|| header.spec.scope != self.scope
			|| header.spec.options != self.options
			|| header.spec.algorithm()?.kind() != self.kind
		{
			return Err(invalid("DiskANN side-file metadata disagrees with the catalog"));
		}
		let delta = match (self.delta_binding, self.delta_file.as_ref()) {
			(None, None) => None,
			(Some(binding), Some(file)) => Some(decode_vector_delta(
				&read_all_pages(store, snapshot, space, binding, file)?,
				header.spec,
			)?),
			_ => {
				return Err(invalid("DiskANN delta binding is only partially registered"));
			}
		};
		search_paged_diskann(
			&header,
			delta.as_ref(),
			query,
			k,
			breadth,
			|ordinals, posting_limit| provider.read_nodes(&header, ordinals, posting_limit),
		)
	}

	fn read(
		&self,
		store: &LocalStore,
		snapshot: &LocalQuerySnapshot,
	) -> FerrosGrynnResult<Arc<PhysicalIndexRead>> {
		let mut decoded = self.decoded.lock();
		if let Some(decoded) = decoded.as_ref() {
			return Ok(Arc::clone(decoded));
		}
		let space = match self.scope {
			IndexScope::Base => crate::datastore::DataSpaceId::Database,
			IndexScope::Effective {
				graph,
			} => crate::datastore::DataSpaceId::Graph(graph),
		};
		let mut read_page = |page: PageNo, output: &mut AlignedPage| {
			let mut loaded = read_snapshot_pages(
				store,
				&snapshot.0,
				space,
				self.binding.file_id,
				&self.file,
				self.file.page_count(),
				&[page],
			)?;
			let page = loaded
				.pages
				.pop()
				.ok_or_else(|| invalid("physical index page read returned no page"))?;
			output.as_bytes_mut().copy_from_slice(page.as_bytes());
			Ok(())
		};
		let entries = match self.kind {
			ClassIndexKind::Standard | ClassIndexKind::Unique => {
				let mode = if self.kind == ClassIndexKind::Standard {
					ScalarIndexMode::Standard
				} else {
					ScalarIndexMode::Unique
				};
				let entries =
					scan_scalar_index::<DiskAtomRef>(&mut read_page, self.binding.root, mode)?
						.into_iter()
						.map(|entry| PhysicalScalarGroup {
							key: entry.key,
							atoms: entry
								.atoms
								.into_iter()
								.map(|atom| PhysicalIndexAtom {
									kind: atom.kind(),
									slot: atom.slot(),
								})
								.collect(),
						})
						.collect();
				PhysicalIndexEntries::Scalar(entries)
			}
			ClassIndexKind::Count => {
				if self.binding.root != PageNo::new(0) {
					return Err(invalid("Count index root is not page zero"));
				}
				let mut page = AlignedPage::zeroed();
				read_page(self.binding.root, &mut page)?;
				let value = decode_count_index(&page)?;
				let expected_scope = match self.scope {
					IndexScope::Base => CountIndexScope::Base,
					IndexScope::Effective {
						graph,
					} => CountIndexScope::Effective(graph),
				};
				if value.class_id != self.class_id.get() || value.scope != expected_scope {
					return Err(invalid("Count index metadata disagrees with the catalog"));
				}
				PhysicalIndexEntries::Count(value.atom_count)
			}
			ClassIndexKind::Fulltext => {
				let pages = read_all_pages(store, snapshot, space, self.binding, &self.file)?;
				let decoded = decode_fulltext_index(&pages)?;
				if decoded.spec.index != self.index_id
					|| decoded.spec.index_version != self.version
					|| decoded.spec.class != self.class_id
					|| decoded.spec.scope != self.scope
					|| self.options != ClassIndexOptions::Fulltext(decoded.spec.options)
				{
					return Err(invalid("Fulltext side-file metadata disagrees with the catalog"));
				}
				PhysicalIndexEntries::Fulltext(decoded)
			}
			ClassIndexKind::Hnsw | ClassIndexKind::DiskAnn => {
				let pages = read_all_pages(store, snapshot, space, self.binding, &self.file)?;
				let decoded = decode_vector_index(&pages)?;
				if decoded.spec.index != self.index_id
					|| decoded.spec.index_version != self.version
					|| decoded.spec.class != self.class_id
					|| decoded.spec.scope != self.scope
					|| decoded.spec.options != self.options
					|| decoded.spec.algorithm()?.kind() != self.kind
				{
					return Err(invalid("vector side-file metadata disagrees with the catalog"));
				}
				PhysicalIndexEntries::Vector(decoded)
			}
		};
		let delta = match (self.delta_binding, self.delta_file.as_ref(), &entries) {
			(None, None, _) => None,
			(Some(binding), Some(file), PhysicalIndexEntries::Fulltext(base)) => {
				let pages = read_all_pages(store, snapshot, space, binding, file)?;
				Some(PhysicalIndexDelta::Fulltext(decode_fulltext_delta(&pages, base.spec)?))
			}
			(Some(binding), Some(file), PhysicalIndexEntries::Vector(base)) => {
				let pages = read_all_pages(store, snapshot, space, binding, file)?;
				Some(PhysicalIndexDelta::Vector(decode_vector_delta(&pages, base.spec)?))
			}
			(Some(_), Some(_), _) => {
				return Err(invalid("scalar or Count index has an unexpected delta binding"));
			}
			_ => return Err(invalid("physical index delta binding is only partially registered")),
		};
		let value = Arc::new(PhysicalIndexRead {
			class_id: self.class_id,
			kind: self.kind,
			scope: self.scope,
			entries,
			delta,
		});
		*decoded = Some(Arc::clone(&value));
		Ok(value)
	}
}

/// Concrete Ferros-Grynn adapter over one captured local storage snapshot.
/// It retains only a bounded query-local validated page window; the shared
/// page cache, WAL view, and durable format remain owned by Ferros-Grynn.
struct FerrosDiskAnnProvider<'store> {
	store: &'store LocalStore,
	snapshot: &'store super::physical_traversal::LocalQuerySnapshotInner,
	space: DataSpaceId,
	binding: PhysicalIndexBinding,
	file: &'store Arc<RegisteredDataFile>,
	pages: VecDeque<(PageNo, CachedVectorPage)>,
}

#[derive(Clone)]
struct CachedVectorPage {
	page: Arc<AlignedPage>,
	header: PageHeader,
}

impl FerrosDiskAnnProvider<'_> {
	fn read_nodes(
		&mut self,
		header: &crate::storage::vector_index::PagedVectorHeader,
		ordinals: &[u32],
		posting_limit: usize,
	) -> FerrosGrynnResult<Vec<PagedVectorNode>> {
		if posting_limit == 0 {
			return Err(invalid("DiskANN posting prefix limit is zero"));
		}
		let locations = self.read_locations(header, ordinals)?;
		let layouts = self.read_layouts(header, &locations)?;
		let page_sets = locations
			.iter()
			.zip(&layouts)
			.map(|(location, layout)| {
				self.node_data_pages(header, *location, *layout, posting_limit)
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let mut nodes = Vec::with_capacity(ordinals.len());
		let mut start = 0;
		'nodes: while start < ordinals.len() {
			let mut end = start;
			let mut pages = BTreeSet::new();
			while end < ordinals.len() {
				let additional =
					page_sets[end].iter().filter(|page| !pages.contains(*page)).count();
				if pages.len().saturating_add(additional) > DISKANN_PAGE_WINDOW {
					if end == start {
						nodes.push(self.read_node(header, ordinals[start], posting_limit)?);
						start += 1;
						continue 'nodes;
					}
					break;
				}
				pages.extend(page_sets[end].iter().copied());
				end += 1;
			}
			let loaded = self.read_page_map(&pages)?;
			for index in start..end {
				nodes.push(Self::decode_node_from_pages(
					header,
					locations[index],
					layouts[index],
					posting_limit,
					&loaded,
				)?);
			}
			self.remember_pages(loaded);
			start = end;
		}
		Ok(nodes)
	}

	fn read_locations(
		&mut self,
		header: &crate::storage::vector_index::PagedVectorHeader,
		ordinals: &[u32],
	) -> FerrosGrynnResult<Vec<VectorNodeLocation>> {
		let addresses = ordinals
			.iter()
			.map(|ordinal| vector_directory_address(header, *ordinal))
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let pages = addresses.iter().map(|(page, _within)| *page).collect::<BTreeSet<_>>();
		let loaded = self.read_page_map(&pages)?;
		let locations = addresses
			.into_iter()
			.map(|(page_no, within)| {
				let page = loaded
					.get(&page_no)
					.ok_or_else(|| invalid("DiskANN directory page was not loaded"))?;
				let payload = decode_validated_vector_stream_page(
					&page.page,
					page_no,
					header.directory,
					page.header,
				)?;
				let entry = payload
					.get(within..within + 16)
					.ok_or_else(|| invalid("DiskANN directory entry crosses its page"))?;
				decode_vector_directory_entry(entry)
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		self.remember_pages(loaded);
		Ok(locations)
	}

	fn read_layouts(
		&mut self,
		header: &crate::storage::vector_index::PagedVectorHeader,
		locations: &[VectorNodeLocation],
	) -> FerrosGrynnResult<Vec<VectorNodeRecordLayout>> {
		let pages = locations.iter().try_fold(BTreeSet::new(), |mut pages, location| {
			pages.extend(Self::stream_range_pages(
				header.stream,
				location.offset,
				VECTOR_NODE_HEADER_BYTES,
			)?);
			Ok::<_, FerrosGrynnError>(pages)
		})?;
		let loaded = self.read_page_map(&pages)?;
		let layouts = locations
			.iter()
			.map(|location| {
				let fixed = Self::read_stream_range_from_pages(
					header.stream,
					location.offset,
					VECTOR_NODE_HEADER_BYTES,
					&loaded,
				)?;
				decode_vector_node_record_layout(&fixed, location.length)
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		self.remember_pages(loaded);
		Ok(layouts)
	}

	fn node_data_pages(
		&self,
		header: &crate::storage::vector_index::PagedVectorHeader,
		location: VectorNodeLocation,
		layout: VectorNodeRecordLayout,
		posting_limit: usize,
	) -> FerrosGrynnResult<BTreeSet<PageNo>> {
		let (posting_start, prefix_bytes, tail_offset, tail_length) =
			Self::node_ranges(location, layout, posting_limit)?;
		let mut pages = Self::stream_range_pages(header.stream, posting_start, prefix_bytes)?;
		pages.extend(Self::stream_range_pages(header.stream, tail_offset, tail_length)?);
		Ok(pages)
	}

	fn decode_node_from_pages(
		header: &crate::storage::vector_index::PagedVectorHeader,
		location: VectorNodeLocation,
		layout: VectorNodeRecordLayout,
		posting_limit: usize,
		pages: &BTreeMap<PageNo, CachedVectorPage>,
	) -> FerrosGrynnResult<PagedVectorNode> {
		let (posting_start, prefix_bytes, tail_offset, tail_length) =
			Self::node_ranges(location, layout, posting_limit)?;
		let postings =
			Self::read_stream_range_from_pages(header.stream, posting_start, prefix_bytes, pages)?;
		let tail =
			Self::read_stream_range_from_pages(header.stream, tail_offset, tail_length, pages)?;
		let mut decoded = decode_vector_node_parts(header.spec, layout, &postings, &tail)?;
		decoded.node.postings.truncate(posting_limit);
		Ok(decoded)
	}

	fn node_ranges(
		location: VectorNodeLocation,
		layout: VectorNodeRecordLayout,
		posting_limit: usize,
	) -> FerrosGrynnResult<(u64, usize, u64, usize)> {
		let prefix_count = layout.posting_count.min(
			posting_limit
				.checked_add(1)
				.ok_or_else(|| invalid("DiskANN posting prefix overflows usize"))?,
		);
		let prefix_bytes = prefix_count
			.checked_mul(8)
			.ok_or_else(|| invalid("DiskANN posting prefix bytes overflow usize"))?;
		let posting_start = location
			.offset
			.checked_add(
				u64::try_from(VECTOR_NODE_HEADER_BYTES)
					.map_err(|_error| invalid("DiskANN node header exceeds u64"))?,
			)
			.ok_or_else(|| invalid("DiskANN posting offset overflows u64"))?;
		let tail_offset = location
			.offset
			.checked_add(
				u64::try_from(layout.postings_end)
					.map_err(|_error| invalid("DiskANN node tail offset exceeds u64"))?,
			)
			.ok_or_else(|| invalid("DiskANN node tail offset overflows u64"))?;
		let tail_length = layout
			.record_length
			.checked_sub(layout.postings_end)
			.ok_or_else(|| invalid("DiskANN node tail length underflows"))?;
		Ok((posting_start, prefix_bytes, tail_offset, tail_length))
	}

	fn read_node(
		&mut self,
		header: &crate::storage::vector_index::PagedVectorHeader,
		ordinal: u32,
		posting_limit: usize,
	) -> FerrosGrynnResult<PagedVectorNode> {
		if posting_limit == 0 {
			return Err(invalid("DiskANN posting prefix limit is zero"));
		}
		let (directory_page, within) = vector_directory_address(header, ordinal)?;
		let location = {
			let page = self.read_page(directory_page)?;
			let payload = decode_validated_vector_stream_page(
				&page.page,
				directory_page,
				header.directory,
				page.header,
			)?;
			let entry = payload
				.get(within..within + 16)
				.ok_or_else(|| invalid("DiskANN directory entry crosses its page"))?;
			decode_vector_directory_entry(entry)?
		};
		let fixed =
			self.read_stream_range(header.stream, location.offset, VECTOR_NODE_HEADER_BYTES)?;
		let layout = decode_vector_node_record_layout(&fixed, location.length)?;
		let prefix_count = layout.posting_count.min(
			posting_limit
				.checked_add(1)
				.ok_or_else(|| invalid("DiskANN posting prefix overflows usize"))?,
		);
		let prefix_bytes = prefix_count
			.checked_mul(8)
			.ok_or_else(|| invalid("DiskANN posting prefix bytes overflow usize"))?;
		let postings = self.read_stream_range(
			header.stream,
			location
				.offset
				.checked_add(
					u64::try_from(VECTOR_NODE_HEADER_BYTES)
						.map_err(|_error| invalid("DiskANN node header exceeds u64"))?,
				)
				.ok_or_else(|| invalid("DiskANN posting offset overflows u64"))?,
			prefix_bytes,
		)?;
		let tail_offset = location
			.offset
			.checked_add(
				u64::try_from(layout.postings_end)
					.map_err(|_error| invalid("DiskANN node tail offset exceeds u64"))?,
			)
			.ok_or_else(|| invalid("DiskANN node tail offset overflows u64"))?;
		let tail_length = layout
			.record_length
			.checked_sub(layout.postings_end)
			.ok_or_else(|| invalid("DiskANN node tail length underflows"))?;
		let tail = self.read_stream_range(header.stream, tail_offset, tail_length)?;
		let mut decoded = decode_vector_node_parts(header.spec, layout, &postings, &tail)?;
		decoded.node.postings.truncate(posting_limit);
		Ok(decoded)
	}

	fn read_page_map(
		&mut self,
		pages: &BTreeSet<PageNo>,
	) -> FerrosGrynnResult<BTreeMap<PageNo, CachedVectorPage>> {
		let mut output = BTreeMap::new();
		let mut missing = Vec::new();
		for page_no in pages {
			if let Some((_page_no, page)) =
				self.pages.iter().find(|(candidate, _page)| candidate == page_no)
			{
				output.insert(*page_no, page.clone());
			} else {
				missing.push(*page_no);
			}
		}
		for window in missing.chunks(DISKANN_PAGE_WINDOW) {
			let loaded = read_snapshot_pages(
				self.store,
				self.snapshot,
				self.space,
				self.binding.file_id,
				self.file,
				self.file.page_count(),
				window,
			)?;
			if loaded.pages.len() != window.len() {
				return Err(invalid("DiskANN page window returned the wrong page count"));
			}
			for (page_no, page) in window.iter().copied().zip(loaded.pages) {
				let header = page.validate(page_no)?;
				let page = CachedVectorPage {
					page: Arc::new(page),
					header,
				};
				if output.insert(page_no, page).is_some() {
					return Err(invalid("DiskANN page window contains a duplicate page"));
				}
			}
		}
		Ok(output)
	}

	fn remember_pages(&mut self, pages: BTreeMap<PageNo, CachedVectorPage>) {
		for (page_no, page) in pages {
			if let Some(position) =
				self.pages.iter().position(|(candidate, _page)| *candidate == page_no)
			{
				self.pages.remove(position);
			}
			if self.pages.len() == DISKANN_PAGE_CACHE_PAGES {
				self.pages.pop_front();
			}
			self.pages.push_back((page_no, page));
		}
	}

	fn stream_range_pages(
		stream: crate::storage::vector_index::StreamLocation,
		start: u64,
		length: usize,
	) -> FerrosGrynnResult<BTreeSet<PageNo>> {
		if length == 0 {
			return Ok(BTreeSet::new());
		}
		let end = start
			.checked_add(
				u64::try_from(length)
					.map_err(|_error| invalid("DiskANN range length exceeds u64"))?,
			)
			.ok_or_else(|| invalid("DiskANN range boundary overflows u64"))?;
		let payload_bytes = u64::try_from(STREAM_PAYLOAD_BYTES)
			.map_err(|_error| invalid("DiskANN page payload exceeds u64"))?;
		let capacity = stream
			.pages
			.checked_mul(payload_bytes)
			.ok_or_else(|| invalid("DiskANN stream capacity overflows u64"))?;
		if end > capacity {
			return Err(invalid("DiskANN range lies outside its data stream"));
		}
		let first = start / payload_bytes;
		let last = (end - 1) / payload_bytes;
		(first..=last)
			.map(|ordinal| {
				stream
					.first
					.checked_add(ordinal)
					.map(PageNo::new)
					.ok_or_else(|| invalid("DiskANN page number overflows u64"))
			})
			.collect()
	}

	fn read_stream_range_from_pages(
		stream: crate::storage::vector_index::StreamLocation,
		start: u64,
		length: usize,
		pages: &BTreeMap<PageNo, CachedVectorPage>,
	) -> FerrosGrynnResult<Vec<u8>> {
		if length == 0 {
			return Ok(Vec::new());
		}
		let end = start
			.checked_add(
				u64::try_from(length)
					.map_err(|_error| invalid("DiskANN range length exceeds u64"))?,
			)
			.ok_or_else(|| invalid("DiskANN range boundary overflows u64"))?;
		let payload_bytes = u64::try_from(STREAM_PAYLOAD_BYTES)
			.map_err(|_error| invalid("DiskANN page payload exceeds u64"))?;
		let mut bytes = Vec::with_capacity(length);
		for page_no in Self::stream_range_pages(stream, start, length)? {
			let page =
				pages.get(&page_no).ok_or_else(|| invalid("DiskANN range page was not loaded"))?;
			let payload =
				decode_validated_vector_stream_page(&page.page, page_no, stream, page.header)?;
			let page_start = (page_no.get() - stream.first)
				.checked_mul(payload_bytes)
				.ok_or_else(|| invalid("DiskANN page offset overflows u64"))?;
			let copy_start = usize::try_from(start.max(page_start) - page_start)
				.map_err(|_error| invalid("DiskANN slice offset exceeds usize"))?;
			let copy_end = usize::try_from(
				end.min(
					page_start
						.checked_add(
							u64::try_from(payload.len())
								.map_err(|_error| invalid("DiskANN payload length exceeds u64"))?,
						)
						.ok_or_else(|| invalid("DiskANN payload boundary overflows u64"))?,
				) - page_start,
			)
			.map_err(|_error| invalid("DiskANN slice end exceeds usize"))?;
			let slice = payload
				.get(copy_start..copy_end)
				.ok_or_else(|| invalid("DiskANN node crosses an incomplete stream page"))?;
			bytes.extend_from_slice(slice);
		}
		if bytes.len() != length {
			return Err(invalid("DiskANN range length disagrees with its request"));
		}
		Ok(bytes)
	}

	fn read_stream_range(
		&mut self,
		stream: crate::storage::vector_index::StreamLocation,
		start: u64,
		length: usize,
	) -> FerrosGrynnResult<Vec<u8>> {
		if length == 0 {
			return Ok(Vec::new());
		}
		let end = start
			.checked_add(
				u64::try_from(length)
					.map_err(|_error| invalid("DiskANN range length exceeds u64"))?,
			)
			.ok_or_else(|| invalid("DiskANN range boundary overflows u64"))?;
		let stream_capacity = stream
			.pages
			.checked_mul(
				u64::try_from(STREAM_PAYLOAD_BYTES)
					.map_err(|_error| invalid("DiskANN page payload exceeds u64"))?,
			)
			.ok_or_else(|| invalid("DiskANN stream capacity overflows u64"))?;
		if end > stream_capacity {
			return Err(invalid("DiskANN range lies outside its data stream"));
		}
		let payload_bytes = u64::try_from(STREAM_PAYLOAD_BYTES)
			.map_err(|_error| invalid("DiskANN page payload exceeds u64"))?;
		let first = start / payload_bytes;
		let last = (end - 1) / payload_bytes;
		let mut record = Vec::with_capacity(length);
		let mut next = first;
		while next <= last {
			let count = usize::try_from(last - next + 1)
				.map_err(|_error| invalid("DiskANN node page span exceeds usize"))?
				.min(DISKANN_PAGE_WINDOW);
			for offset in 0..count {
				let offset = u64::try_from(offset)
					.map_err(|_error| invalid("DiskANN page offset exceeds u64"))?;
				let page_no = stream
					.first
					.checked_add(next)
					.and_then(|page| page.checked_add(offset))
					.map(PageNo::new)
					.ok_or_else(|| invalid("DiskANN page number overflows u64"))?;
				let page = self.read_page(page_no)?;
				let payload =
					decode_validated_vector_stream_page(&page.page, page_no, stream, page.header)?;
				let page_ordinal = page_no.get() - stream.first;
				let page_start = page_ordinal
					.checked_mul(
						u64::try_from(STREAM_PAYLOAD_BYTES)
							.map_err(|_error| invalid("DiskANN page payload exceeds u64"))?,
					)
					.ok_or_else(|| invalid("DiskANN page offset overflows u64"))?;
				let copy_start = start.max(page_start) - page_start;
				let copy_end = end.min(
					page_start
						.checked_add(
							u64::try_from(payload.len())
								.map_err(|_error| invalid("DiskANN payload length exceeds u64"))?,
						)
						.ok_or_else(|| invalid("DiskANN payload boundary overflows u64"))?,
				) - page_start;
				if copy_start > copy_end
					|| usize::try_from(copy_end).ok().is_none_or(|value| value > payload.len())
				{
					return Err(invalid("DiskANN node crosses an incomplete stream page"));
				}
				record.extend_from_slice(
					&payload[usize::try_from(copy_start)
						.map_err(|_error| invalid("DiskANN slice offset exceeds usize"))?
						..usize::try_from(copy_end)
							.map_err(|_error| invalid("DiskANN slice end exceeds usize"))?],
				);
			}
			next = next
				.checked_add(
					u64::try_from(count)
						.map_err(|_error| invalid("DiskANN page window exceeds u64"))?,
				)
				.ok_or_else(|| invalid("DiskANN page cursor overflows u64"))?;
		}
		if record.len() != length {
			return Err(invalid("DiskANN range length disagrees with its request"));
		}
		Ok(record)
	}

	fn read_page(&mut self, page: PageNo) -> FerrosGrynnResult<&CachedVectorPage> {
		if let Some(position) = self.pages.iter().position(|(candidate, _page)| *candidate == page)
		{
			let cached = self
				.pages
				.remove(position)
				.ok_or_else(|| invalid("DiskANN page window lost a resident page"))?;
			self.pages.push_back(cached);
			return self
				.pages
				.back()
				.map(|(_page_no, page)| page)
				.ok_or_else(|| invalid("DiskANN page window is unexpectedly empty"));
		}
		let mut loaded = read_snapshot_pages(
			self.store,
			self.snapshot,
			self.space,
			self.binding.file_id,
			self.file,
			self.file.page_count(),
			&[page],
		)?;
		if loaded.pages.len() != 1 {
			return Err(invalid("DiskANN page read returned the wrong page count"));
		}
		if self.pages.len() == DISKANN_PAGE_CACHE_PAGES {
			self.pages.pop_front();
		}
		let loaded = loaded.pages.remove(0);
		let header = loaded.validate(page)?;
		self.pages.push_back((page, CachedVectorPage {
			page: Arc::new(loaded),
			header,
		}));
		self.pages
			.back()
			.map(|(_page_no, page)| page)
			.ok_or_else(|| invalid("DiskANN page window is unexpectedly empty"))
	}
}

fn read_all_pages(
	store: &LocalStore,
	snapshot: &LocalQuerySnapshot,
	space: DataSpaceId,
	binding: PhysicalIndexBinding,
	file: &Arc<RegisteredDataFile>,
) -> FerrosGrynnResult<Vec<AlignedPage>> {
	let count = file.page_count();
	let capacity = usize::try_from(count)
		.map_err(|_error| invalid("physical index page count exceeds usize"))?;
	let mut pages = Vec::with_capacity(capacity);
	let numbers = (0..count).map(PageNo::new).collect::<Vec<_>>();
	for window in numbers.chunks(256) {
		let mut loaded =
			read_snapshot_pages(store, &snapshot.0, space, binding.file_id, file, count, window)?;
		pages.append(&mut loaded.pages);
	}
	Ok(pages)
}

/// Binds catalog identities to lazy registered handles without opening the
/// underlying native files.
pub(super) fn bind_physical_indexes(
	catalog: &DatabaseCatalog,
	registry: &FileRegistry,
) -> FerrosGrynnResult<BTreeMap<IndexId, Arc<PhysicalIndexFile>>> {
	let mut files = BTreeMap::new();
	for class in &catalog.classes {
		for index in &class.indexes {
			let Some(binding) = index.physical else {
				continue;
			};
			let space = match index.scope {
				IndexScope::Base => crate::datastore::DataSpaceId::Database,
				IndexScope::Effective {
					graph,
				} => crate::datastore::DataSpaceId::Graph(graph),
			};
			let file = Arc::new(PhysicalIndexFile {
				index_id: index.id,
				version: index.version,
				class_id: class.id,
				kind: index.kind,
				scope: index.scope,
				options: index.options,
				binding,
				file: registry.resolve_file(space, binding.file_id)?,
				delta_binding: index.delta,
				delta_file: index
					.delta
					.map(|delta| registry.resolve_file(space, delta.file_id))
					.transpose()?,
				decoded: Mutex::new(None),
			});
			if files.insert(index.id, file).is_some() {
				return Err(invalid("duplicate physical IndexId in database catalog"));
			}
		}
	}
	Ok(files)
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

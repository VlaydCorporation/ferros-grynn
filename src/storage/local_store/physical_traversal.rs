#![allow(
	clippy::missing_docs_in_private_items,
	clippy::redundant_pub_crate,
	reason = "private page-decoding helpers implement the documented snapshot-exact cursor \
	          contract"
)]

//! Snapshot-exact hot-page reads for the local traversal cursor.

use std::{
	collections::{BTreeMap, BTreeSet},
	sync::Arc,
};

use rapidhash::RapidHashMap;

use super::{
	GraphBinding,
	LocalStore,
	physical_index::PhysicalIndexFile,
	statistics::PhysicalStatisticsFiles,
};
#[cfg(all(windows, feature = "native-async-io"))]
use crate::storage::hybrid_engine::{ReadIntent, ReadWindow, TransportCapabilities};
use crate::{
	datastore::{ClassId, DataSpaceId, GlobalAtomId, PropertyOverlay},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, AtomKind, EdgeDirection, EndpointRole, GraphId, PathDirection},
	storage::{
		atom_page::{AtomHotSlot, PropertyRef, atom_location, read_atom_hot_slot},
		atom_store_image::AtomStoreFileSet,
		atom_store_superblock::AtomStoreSuperblock,
		file_registry::RegisteredDataFile,
		graph_pages::{
			EdgeHotSlot,
			EdgeSlotFlags,
			EdgeTopologyCode,
			NodeHotSlot,
			NodeSlotFlags,
			decode_adjacency_overflow,
			decode_overlay,
			hot_slot_address,
			read_edge_hot_slot,
			read_node_hot_slot,
		},
		graph_store::GraphFileSet,
		graph_superblock::GraphSuperblock,
		hybrid_engine::MAX_READ_SCOPE_OPERATIONS,
		page::{AlignedPage, PageNo},
		page_cache::{PageCacheKey, PageCacheVersion},
		property_page::{
			LARGE_CHUNK_BYTES,
			MAX_INLINE_BYTES,
			decode_large_overlay,
			decode_large_property,
			decode_property_map,
			decode_record_id,
			read_overlay_record,
			read_property_record,
		},
		types::{DataFileId, PageKey, StorageSnapshot},
		wal::TxHandle,
	},
	values::{
		PropertyMap,
		RecordId,
		storage::{DecodeContext, DiskAtomRef, codec::DecodeLimits},
	},
};

/// Storage-transport observations for one logical frontier expansion.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PhysicalTraversalReadStats {
	pub(crate) resident_pages: u64,
	pub(crate) wal_pages: u64,
	pub(crate) sync_home_pages: u64,
	pub(crate) ioring_home_pages: u64,
}

impl PhysicalTraversalReadStats {
	fn add(&mut self, other: Self) {
		self.resident_pages = self.resident_pages.saturating_add(other.resident_pages);
		self.wal_pages = self.wal_pages.saturating_add(other.wal_pages);
		self.sync_home_pages = self.sync_home_pages.saturating_add(other.sync_home_pages);
		self.ioring_home_pages = self.ioring_home_pages.saturating_add(other.ioring_home_pages);
	}
}

/// One owned same-level incidence decoded from graph hot pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PhysicalTraversalIncidence {
	pub(crate) target: AtomId,
	pub(crate) edge: AtomId,
	pub(crate) direction: PathDirection,
	pub(crate) role: EndpointRole,
	pub(crate) topology: EdgeDirection,
}

/// Complete physical result or an explicit request to use materialized
/// fallback.
pub(crate) struct PhysicalTraversalRead {
	pub(crate) incidences: Vec<PhysicalTraversalIncidence>,
	pub(crate) stats: PhysicalTraversalReadStats,
}

/// Exact physical resolution of one meta owner's nested graph.
pub(crate) struct PhysicalTraversalInnerRead {
	pub(crate) graph: Option<GraphId>,
	pub(crate) stats: PhysicalTraversalReadStats,
}

/// Exact base and graph-local property state decoded from physical pages.
pub(crate) struct PhysicalTraversalFieldsRead {
	pub(crate) global: GlobalAtomId,
	pub(crate) class_id: ClassId,
	pub(crate) record_id: RecordId,
	pub(crate) base_properties: PropertyMap,
	pub(crate) overlay: Option<PropertyOverlay>,
	pub(crate) stats: PhysicalTraversalReadStats,
}

/// Immutable database-global AtomStore binding for one query snapshot.
struct PhysicalAtomSnapshot {
	files: AtomStoreFileSet,
	superblock: AtomStoreSuperblock,
	atoms_hot: Arc<RegisteredDataFile>,
	properties: Arc<RegisteredDataFile>,
}

/// Immutable physical binding retained with a registered WAL snapshot.
struct PhysicalGraphSnapshot {
	files: GraphFileSet,
	superblock: GraphSuperblock,
	arena: crate::storage::graph_pages::GraphArenaBindings,
	nodes: Arc<RegisteredDataFile>,
	edges: Arc<RegisteredDataFile>,
	adjacency: Arc<RegisteredDataFile>,
	overlays: Arc<RegisteredDataFile>,
}

/// Cloneable token keeping the exact WAL visibility boundary alive.
pub(super) struct LocalQuerySnapshotInner {
	pub(super) _pin: TxHandle,
	pub(super) snapshot: StorageSnapshot,
	pub(super) home_generation: crate::storage::types::CheckpointGeneration,
	pub(super) home_ceiling: crate::storage::types::Lsn,
	atoms: PhysicalAtomSnapshot,
	graphs: BTreeMap<GraphId, PhysicalGraphSnapshot>,
	pub(super) indexes: BTreeMap<crate::datastore::IndexId, Arc<PhysicalIndexFile>>,
	pub(super) statistics: Option<Arc<PhysicalStatisticsFiles>>,
}

/// Local physical state captured for one immutable query snapshot.
#[derive(Clone)]
pub(crate) struct LocalQuerySnapshot(pub(super) Arc<LocalQuerySnapshotInner>);

/// One bounded physical traversal reader. It never exposes pages or file ids
/// above the datastore boundary.
struct PhysicalTraversalCursor<'store> {
	store: &'store LocalStore,
	snapshot: &'store LocalQuerySnapshotInner,
	graph: &'store PhysicalGraphSnapshot,
}

impl LocalStore {
	/// Captures current physical bindings only when they exactly match the MVCC
	/// root. Historical and transaction-private roots deliberately fall back to
	/// their materialized graph view.
	pub(crate) fn capture_query_snapshot(
		&self,
		commit_ts: u64,
	) -> FerrosGrynnResult<Option<LocalQuerySnapshot>> {
		let state = self.state.lock();
		let pin = self.wal.begin_tx()?;
		let snapshot = pin.snapshot();
		if snapshot.commit_ts().get() != commit_ts {
			return Ok(None);
		}
		let home_generation = state.database_superblock.generation;
		let home_ceiling = state.database_superblock.checkpoint_lsn;
		let atoms =
			physical_atom_snapshot(&state.registry, state.bindings.atoms, state.atom_superblock)?;
		let mut graphs = BTreeMap::new();
		for binding in &state.bindings.graphs {
			let superblock = state
				.graph_superblocks
				.get(&binding.graph_id)
				.copied()
				.ok_or_else(|| invalid("physical graph binding has no GraphSuperblock"))?;
			graphs.insert(
				binding.graph_id,
				physical_graph_snapshot(&state.registry, binding, superblock)?,
			);
		}
		Ok(Some(LocalQuerySnapshot(Arc::new(LocalQuerySnapshotInner {
			_pin: pin,
			snapshot,
			home_generation,
			home_ceiling,
			atoms,
			graphs,
			indexes: state.physical_indexes.clone(),
			statistics: state.physical_statistics.clone(),
		}))))
	}

	/// Expands one local node from exact hot-page state. Unsupported edge
	/// shapes return `None` before publishing any partial logical output.
	pub(crate) fn physical_traversal_expand(
		&self,
		snapshot: &LocalQuerySnapshot,
		graph: GraphId,
		source: AtomId,
		forward: bool,
		backward: bool,
		undirected: bool,
	) -> FerrosGrynnResult<Option<PhysicalTraversalRead>> {
		let Some(graph) = snapshot.0.graphs.get(&graph) else {
			return Ok(None);
		};
		PhysicalTraversalCursor {
			store: self,
			snapshot: &snapshot.0,
			graph,
		}
		.expand_inline(source, forward, backward, undirected)
	}

	/// Resolves one node inner graph or edge transition graph from the same
	/// snapshot-exact hot-page source used by frontier expansion.
	pub(crate) fn physical_traversal_inner_graph(
		&self,
		snapshot: &LocalQuerySnapshot,
		graph: GraphId,
		owner: AtomId,
	) -> FerrosGrynnResult<Option<PhysicalTraversalInnerRead>> {
		let Some(graph) = snapshot.0.graphs.get(&graph) else {
			return Ok(None);
		};
		PhysicalTraversalCursor {
			store: self,
			snapshot: &snapshot.0,
			graph,
		}
		.inner_graph(owner)
	}

	/// Resolves one occurrence's base and graph-local property records from
	/// exact snapshot-visible files.
	pub(crate) fn physical_traversal_fields(
		&self,
		snapshot: &LocalQuerySnapshot,
		graph: GraphId,
		owner: AtomId,
		context: &DecodeContext<'_>,
	) -> FerrosGrynnResult<Option<PhysicalTraversalFieldsRead>> {
		let Some(graph) = snapshot.0.graphs.get(&graph) else {
			return Ok(None);
		};
		PhysicalTraversalCursor {
			store: self,
			snapshot: &snapshot.0,
			graph,
		}
		.fields(owner, context)
	}
}

impl PhysicalTraversalCursor<'_> {
	fn fields(
		&self,
		owner: AtomId,
		context: &DecodeContext<'_>,
	) -> FerrosGrynnResult<Option<PhysicalTraversalFieldsRead>> {
		let mut stats = PhysicalTraversalReadStats::default();
		let (page_no, within) = hot_slot_address(owner.slot())?;
		let (global, overlay) = match owner.kind() {
			Some(AtomKind::Node) => {
				let loaded = self.read_pages(
					self.graph.files.space(),
					self.graph.files.nodes,
					&self.graph.nodes,
					self.graph.superblock.node_hot_pages,
					&[page_no],
				)?;
				stats.add(loaded.stats);
				let slot = read_node_hot_slot(&loaded.pages[0], page_no, within)?;
				validate_node_identity(&slot, owner)?;
				(
					slot.global_atom
						.ok_or_else(|| invalid("validated node has no global atom identity"))?,
					slot.overlay,
				)
			}
			Some(AtomKind::Edge) => {
				let loaded = self.read_pages(
					self.graph.files.space(),
					self.graph.files.edges,
					&self.graph.edges,
					self.graph.superblock.edge_hot_pages,
					&[page_no],
				)?;
				stats.add(loaded.stats);
				let slot = read_edge_hot_slot(&loaded.pages[0], page_no, within)?;
				validate_edge_identity(&slot, owner)?;
				(
					slot.global_atom
						.ok_or_else(|| invalid("validated edge has no global atom identity"))?,
					slot.overlay,
				)
			}
			None => return Ok(None),
		};
		let (atom_page_no, atom_within) = atom_location(global.slot());
		let loaded = self.read_pages(
			DataSpaceId::Database,
			self.snapshot.atoms.files.atoms_hot,
			&self.snapshot.atoms.atoms_hot,
			self.snapshot.atoms.superblock.atoms_hot_pages,
			&[atom_page_no],
		)?;
		stats.add(loaded.stats);
		let (decoded_global, slot) =
			read_atom_hot_slot(&loaded.pages[0], atom_page_no, atom_within)?;
		if decoded_global != global {
			return Err(invalid("graph occurrence and AtomStore hot slot disagree"));
		}
		let AtomHotSlot::Live {
			kind,
			class_id,
			record_id,
			properties,
		} = slot
		else {
			return Err(invalid("graph occurrence references an AtomStore tombstone"));
		};
		if Some(kind) != owner.kind()
			|| bounded_record_pages(record_id, properties, overlay)? > MAX_READ_SCOPE_OPERATIONS
		{
			return Ok(None);
		}

		let record = self
			.read_record(
				DataSpaceId::Database,
				self.snapshot.atoms.files.properties,
				&self.snapshot.atoms.properties,
				self.snapshot.atoms.superblock.properties_pages,
				record_id,
				false,
				&mut stats,
			)?
			.ok_or_else(|| invalid("live AtomStore slot has no RecordId record"))?;
		let record_id = decode_record_id(&record, context, DecodeLimits::default())?;
		let base_properties = match self.read_record(
			DataSpaceId::Database,
			self.snapshot.atoms.files.properties,
			&self.snapshot.atoms.properties,
			self.snapshot.atoms.superblock.properties_pages,
			properties,
			false,
			&mut stats,
		)? {
			Some(record) => decode_property_map(&record, context, DecodeLimits::default())?,
			None => PropertyMap::new(),
		};
		let overlay = match self.read_record(
			self.graph.files.space(),
			self.graph.files.overlays,
			&self.graph.overlays,
			self.graph.superblock.overlays_pages,
			overlay,
			true,
			&mut stats,
		)? {
			Some(record) => Some(decode_overlay(&record, context, DecodeLimits::default())?),
			None => None,
		};
		Ok(Some(PhysicalTraversalFieldsRead {
			global,
			class_id,
			record_id,
			base_properties,
			overlay,
			stats,
		}))
	}

	#[allow(
		clippy::too_many_arguments,
		reason = "one record read needs an exact source identity and page ceiling"
	)]
	fn read_record(
		&self,
		space: DataSpaceId,
		file_id: DataFileId,
		file: &Arc<RegisteredDataFile>,
		page_ceiling: u64,
		reference: PropertyRef,
		overlay: bool,
		stats: &mut PhysicalTraversalReadStats,
	) -> FerrosGrynnResult<Option<Vec<u8>>> {
		match reference {
			PropertyRef::None => Ok(None),
			PropertyRef::Inline {
				page,
				slot,
				length,
			} => {
				if usize::try_from(length).ok().is_none_or(|length| length > MAX_INLINE_BYTES) {
					return Err(invalid("inline property reference exceeds its format limit"));
				}
				let loaded = self.read_pages(space, file_id, file, page_ceiling, &[page])?;
				stats.add(loaded.stats);
				let record = if overlay {
					read_overlay_record(&loaded.pages[0], page, slot, length)?
				} else {
					read_property_record(&loaded.pages[0], page, slot, length)?
				};
				Ok(Some(record.to_vec()))
			}
			PropertyRef::Large {
				first_page,
				length,
			} => {
				let length_usize = usize::try_from(length)
					.map_err(|_error| invalid("large property length exceeds usize"))?;
				if length_usize <= MAX_INLINE_BYTES {
					return Err(invalid("large property reference fits the inline format"));
				}
				let count = length_usize.div_ceil(LARGE_CHUNK_BYTES);
				let pages = contiguous_pages(first_page, count)?;
				let loaded = self.read_pages(space, file_id, file, page_ceiling, &pages)?;
				stats.add(loaded.stats);
				if overlay {
					decode_large_overlay(
						first_page,
						&loaded.pages,
						length,
						DecodeLimits::default().max_total_allocation,
					)
					.map(Some)
				} else {
					decode_large_property(
						first_page,
						&loaded.pages,
						length,
						DecodeLimits::default().max_total_allocation,
					)
					.map(Some)
				}
			}
		}
	}

	fn inner_graph(&self, owner: AtomId) -> FerrosGrynnResult<Option<PhysicalTraversalInnerRead>> {
		let mut stats = PhysicalTraversalReadStats::default();
		let (page_no, within) = hot_slot_address(owner.slot())?;
		let arena_slot = match owner.kind() {
			Some(AtomKind::Node) => {
				let loaded = self.read_pages(
					self.graph.files.space(),
					self.graph.files.nodes,
					&self.graph.nodes,
					self.graph.superblock.node_hot_pages,
					&[page_no],
				)?;
				stats.add(loaded.stats);
				let slot = read_node_hot_slot(&loaded.pages[0], page_no, within)?;
				validate_node_identity(&slot, owner)?;
				slot.subgraph_slot
			}
			Some(AtomKind::Edge) => {
				let loaded = self.read_pages(
					self.graph.files.space(),
					self.graph.files.edges,
					&self.graph.edges,
					self.graph.superblock.edge_hot_pages,
					&[page_no],
				)?;
				stats.add(loaded.stats);
				let slot = read_edge_hot_slot(&loaded.pages[0], page_no, within)?;
				validate_edge_identity(&slot, owner)?;
				slot.transition_subgraph_slot
			}
			None => return Ok(None),
		};
		let graph = arena_slot
			.map(|slot| {
				self.graph.arena.graph_by_slot.get(&slot).copied().ok_or_else(|| {
					invalid("physical meta owner references an absent graph arena slot")
				})
			})
			.transpose()?;
		Ok(Some(PhysicalTraversalInnerRead {
			graph,
			stats,
		}))
	}

	fn expand_inline(
		&self,
		source: AtomId,
		forward: bool,
		backward: bool,
		undirected: bool,
	) -> FerrosGrynnResult<Option<PhysicalTraversalRead>> {
		if source.kind() != Some(AtomKind::Node) {
			return Ok(None);
		}
		let mut stats = PhysicalTraversalReadStats::default();
		let (source_page, source_within) = hot_slot_address(source.slot())?;
		let source_pages = self.read_pages(
			self.graph.files.space(),
			self.graph.files.nodes,
			&self.graph.nodes,
			self.graph.superblock.node_hot_pages,
			&[source_page],
		)?;
		stats.add(source_pages.stats);
		let source_slot = read_node_hot_slot(&source_pages.pages[0], source_page, source_within)?;
		validate_node_identity(&source_slot, source)?;

		let mut requested = Vec::new();
		if forward
			&& !self.append_node_side(
				&mut requested,
				&source_slot.out_inline,
				source_slot.out_count,
				source_slot.out_overflow,
				RequestedDirection::Forward,
				&mut stats,
			)? {
			return Ok(None);
		}
		if backward
			&& !self.append_node_side(
				&mut requested,
				&source_slot.in_inline,
				source_slot.in_count,
				source_slot.in_overflow,
				RequestedDirection::Backward,
				&mut stats,
			)? {
			return Ok(None);
		}
		if undirected
			&& !self.append_node_side(
				&mut requested,
				&source_slot.undirected_inline,
				source_slot.undirected_count,
				source_slot.undirected_overflow,
				RequestedDirection::Undirected,
				&mut stats,
			)? {
			return Ok(None);
		}
		let source_ref = DiskAtomRef::from_parts(AtomKind::Node, source.slot())?;
		let edge_references = requested.iter().map(|(edge, _direction)| *edge).collect::<Vec<_>>();
		let edge_pages = unique_hot_pages(&edge_references)?;
		let loaded_edges = self.read_pages(
			self.graph.files.space(),
			self.graph.files.edges,
			&self.graph.edges,
			self.graph.superblock.edge_hot_pages,
			&edge_pages,
		)?;
		stats.add(loaded_edges.stats);
		let edge_page_index = page_index(&edge_pages);
		let mut decoded = Vec::with_capacity(requested.len());
		let mut target_refs = Vec::with_capacity(requested.len());
		for (edge_ref, requested_direction) in requested {
			let (page_no, within) = hot_slot_address(edge_ref.slot())?;
			let page = loaded_edges
				.pages
				.get(edge_page_index[&page_no])
				.ok_or_else(|| invalid("physical edge hot-page window lost its page mapping"))?;
			let slot = read_edge_hot_slot(page, page_no, within)?;
			let Some(candidate) =
				inline_candidate(&slot, edge_ref, source_ref, requested_direction)?
			else {
				return Ok(None);
			};
			target_refs.push(candidate.target);
			decoded.push((edge_ref, slot, candidate));
		}
		if target_refs.iter().any(|target| target.kind() != AtomKind::Node) {
			return Ok(None);
		}

		let target_pages = unique_hot_pages(&target_refs)?;
		let loaded_targets = self.read_pages(
			self.graph.files.space(),
			self.graph.files.nodes,
			&self.graph.nodes,
			self.graph.superblock.node_hot_pages,
			&target_pages,
		)?;
		stats.add(loaded_targets.stats);
		let target_page_index = page_index(&target_pages);
		let mut incidences = Vec::with_capacity(decoded.len());
		for (edge_ref, edge_slot, candidate) in decoded {
			let (page_no, within) = hot_slot_address(candidate.target.slot())?;
			let page = loaded_targets
				.pages
				.get(target_page_index[&page_no])
				.ok_or_else(|| invalid("physical target hot-page window lost its page mapping"))?;
			let target_slot = read_node_hot_slot(page, page_no, within)?;
			if target_slot.flags.contains(NodeSlotFlags::TOMBSTONE) {
				return Err(invalid("physical edge endpoint references a node tombstone"));
			}
			let target = AtomId::from_parts(
				AtomKind::Node,
				candidate.target.slot(),
				target_slot.generation,
			)?;
			let edge = AtomId::from_parts(AtomKind::Edge, edge_ref.slot(), edge_slot.generation)?;
			incidences.push(PhysicalTraversalIncidence {
				target,
				edge,
				direction: candidate.direction,
				role: candidate.role,
				topology: candidate.topology,
			});
		}
		let mut unique = Vec::with_capacity(incidences.len());
		for incidence in incidences {
			if !unique.contains(&incidence) {
				unique.push(incidence);
			}
		}
		Ok(Some(PhysicalTraversalRead {
			incidences: unique,
			stats,
		}))
	}

	fn append_node_side<const N: usize>(
		&self,
		output: &mut Vec<(DiskAtomRef, RequestedDirection)>,
		inline: &[Option<DiskAtomRef>; N],
		count: u32,
		overflow: Option<PageNo>,
		direction: RequestedDirection,
		stats: &mut PhysicalTraversalReadStats,
	) -> FerrosGrynnResult<bool> {
		let count = usize::try_from(count).map_err(|error| {
			invalid(format!("physical adjacency count does not fit usize: {error}"))
		})?;
		for entry in inline.iter().take(count.min(N)) {
			let Some(entry) = entry else {
				return Err(invalid("physical adjacency has an absent inline edge"));
			};
			output.push((*entry, direction));
		}
		if count <= N {
			return Ok(overflow.is_none());
		}
		let Some(first) = overflow else {
			return Err(invalid("physical adjacency count requires an overflow chain"));
		};
		let expected = count - N;
		let Some(entries) = self.read_adjacency_chain(first, expected, stats)? else {
			return Ok(false);
		};
		output.extend(entries.into_iter().map(|entry| (entry, direction)));
		Ok(true)
	}

	fn read_adjacency_chain(
		&self,
		first: PageNo,
		expected: usize,
		stats: &mut PhysicalTraversalReadStats,
	) -> FerrosGrynnResult<Option<Vec<DiskAtomRef>>> {
		let mut entries = Vec::with_capacity(expected);
		let mut current = Some(first);
		let mut visited = BTreeSet::new();
		while let Some(page_no) = current {
			if visited.len() >= MAX_READ_SCOPE_OPERATIONS {
				return Ok(None);
			}
			if !visited.insert(page_no) {
				return Err(invalid("physical adjacency overflow chain contains a cycle"));
			}
			let loaded = self.read_pages(
				self.graph.files.space(),
				self.graph.files.adjacency,
				&self.graph.adjacency,
				self.graph.superblock.adjacency_overflow_pages,
				&[page_no],
			)?;
			stats.add(loaded.stats);
			let Some(page) = loaded.pages.first() else {
				return Err(invalid("physical adjacency page window is empty"));
			};
			let (next, page_entries) = decode_adjacency_overflow(page, page_no)?;
			entries.extend(page_entries);
			if entries.len() > expected {
				return Err(invalid("physical adjacency overflow exceeds its hot-slot count"));
			}
			current = next;
		}
		if entries.len() != expected {
			return Err(invalid("physical adjacency overflow disagrees with its hot-slot count"));
		}
		Ok(Some(entries))
	}

	fn read_pages(
		&self,
		space: DataSpaceId,
		file_id: DataFileId,
		file: &Arc<RegisteredDataFile>,
		page_ceiling: u64,
		page_numbers: &[PageNo],
	) -> FerrosGrynnResult<LoadedPages> {
		read_snapshot_pages(
			self.store,
			self.snapshot,
			space,
			file_id,
			file,
			page_ceiling,
			page_numbers,
		)
	}
}

/// Reads one bounded exact-version page window shared by traversal and lazy
/// immutable index decoding.
pub(super) fn read_snapshot_pages(
	store: &LocalStore,
	snapshot: &LocalQuerySnapshotInner,
	space: DataSpaceId,
	file_id: DataFileId,
	file: &Arc<RegisteredDataFile>,
	page_ceiling: u64,
	page_numbers: &[PageNo],
) -> FerrosGrynnResult<LoadedPages> {
	if page_numbers.len() > MAX_READ_SCOPE_OPERATIONS
		|| page_numbers.iter().any(|page| page.get() >= page_ceiling)
	{
		return Err(invalid("physical query page window exceeds its bounded source"));
	}
	let keys =
		page_numbers.iter().map(|page| PageKey::new(space, file_id, *page)).collect::<Vec<_>>();
	let visible = store.wal.visible_frames(&keys, snapshot.snapshot);
	let mut pages = (0..keys.len()).map(|_| AlignedPage::zeroed()).collect::<Vec<_>>();
	let mut cold_home = Vec::new();
	let mut stats = PhysicalTraversalReadStats::default();
	for (index, (key, frame)) in keys.iter().zip(visible.frames()).enumerate() {
		let version = frame.map_or(
			PageCacheVersion::Home {
				generation: snapshot.home_generation,
				ceiling: snapshot.home_ceiling,
			},
			|frame| {
				PageCacheVersion::Wal(crate::storage::page::PageLsn::new(frame.frame_lsn().get()))
			},
		);
		let cache_key = PageCacheKey::new(*key, version);
		if let Some(resident) = store.engine.get_if_resident(cache_key)? {
			pages[index].as_bytes_mut().copy_from_slice(resident.as_bytes());
			stats.resident_pages = stats.resident_pages.saturating_add(1);
			continue;
		}
		if let Some(frame) = frame {
			let loaded =
				store.engine.get_or_load(cache_key, |output| visible.read_frame(*frame, output))?;
			pages[index].as_bytes_mut().copy_from_slice(loaded.as_bytes());
			stats.wal_pages = stats.wal_pages.saturating_add(1);
		} else {
			cold_home.push(index);
		}
	}
	if !cold_home.is_empty() {
		read_cold_home(store, file, page_numbers, &cold_home, &mut pages, &mut stats)?;
	}
	Ok(LoadedPages {
		pages,
		stats,
	})
}

fn read_cold_home(
	store: &LocalStore,
	file: &Arc<RegisteredDataFile>,
	page_numbers: &[PageNo],
	cold: &[usize],
	pages: &mut [AlignedPage],
	stats: &mut PhysicalTraversalReadStats,
) -> FerrosGrynnResult<()> {
	#[cfg(not(all(windows, feature = "native-async-io")))]
	let _ = store;
	let direct = file.file()?;
	let cold_count = u64::try_from(cold.len())
		.map_err(|error| invalid(format!("physical cold-page count does not fit u64: {error}")))?;
	#[cfg(all(windows, feature = "native-async-io"))]
	let route = store.engine.route(
		false,
		ReadIntent::IndexTraversal,
		ReadWindow::Sparse {
			operations: page_numbers.len(),
			cold_pages: cold.len(),
		},
		store.io.mode(),
		TransportCapabilities::current(),
	);
	#[cfg(all(windows, feature = "native-async-io"))]
	if matches!(route, crate::storage::hybrid_engine::ReadRoute::IoRingSparse { .. })
		&& cold.len() <= store.engine.sparse_window_capacity()
	{
		let reads = cold
			.iter()
			.map(|index| (Arc::clone(&direct), page_numbers[*index]))
			.collect::<Vec<_>>();
		let loaded = store.engine.read_sparse_ioring(&reads)?;
		for (position, index) in cold.iter().copied().enumerate() {
			pages[index].as_bytes_mut().copy_from_slice(loaded.pages()[position].as_bytes());
		}
		stats.ioring_home_pages = stats.ioring_home_pages.saturating_add(cold_count);
		return Ok(());
	}
	for index in cold {
		direct.read_page(page_numbers[*index], &mut pages[*index])?;
	}
	stats.sync_home_pages = stats.sync_home_pages.saturating_add(cold_count);
	Ok(())
}

pub(super) struct LoadedPages {
	pub(super) pages: Vec<AlignedPage>,
	pub(super) stats: PhysicalTraversalReadStats,
}

#[derive(Clone, Copy)]
struct EncodedCandidate {
	target: DiskAtomRef,
	direction: PathDirection,
	role: EndpointRole,
	topology: EdgeDirection,
}

#[derive(Clone, Copy)]
enum RequestedDirection {
	Forward,
	Backward,
	Undirected,
}

fn physical_graph_snapshot(
	registry: &crate::storage::file_registry::FileRegistry,
	binding: &GraphBinding,
	superblock: GraphSuperblock,
) -> FerrosGrynnResult<PhysicalGraphSnapshot> {
	Ok(PhysicalGraphSnapshot {
		files: binding.files,
		superblock,
		arena: binding.arena.clone(),
		nodes: registry.resolve_file(binding.files.space(), binding.files.nodes)?,
		edges: registry.resolve_file(binding.files.space(), binding.files.edges)?,
		adjacency: registry.resolve_file(binding.files.space(), binding.files.adjacency)?,
		overlays: registry.resolve_file(binding.files.space(), binding.files.overlays)?,
	})
}

fn physical_atom_snapshot(
	registry: &crate::storage::file_registry::FileRegistry,
	files: AtomStoreFileSet,
	superblock: AtomStoreSuperblock,
) -> FerrosGrynnResult<PhysicalAtomSnapshot> {
	Ok(PhysicalAtomSnapshot {
		files,
		superblock,
		atoms_hot: registry.resolve_file(DataSpaceId::Database, files.atoms_hot)?,
		properties: registry.resolve_file(DataSpaceId::Database, files.properties)?,
	})
}

fn bounded_record_pages(
	record_id: PropertyRef,
	properties: PropertyRef,
	overlay: PropertyRef,
) -> FerrosGrynnResult<usize> {
	[record_id, properties, overlay].into_iter().map(record_page_count).try_fold(
		2_usize,
		|total, count| {
			total
				.checked_add(count?)
				.ok_or_else(|| invalid("physical field page count overflows usize"))
		},
	)
}

fn record_page_count(reference: PropertyRef) -> FerrosGrynnResult<usize> {
	match reference {
		PropertyRef::None => Ok(0),
		PropertyRef::Inline {
			..
		} => Ok(1),
		PropertyRef::Large {
			length,
			..
		} => usize::try_from(length)
			.map(|length| length.div_ceil(LARGE_CHUNK_BYTES))
			.map_err(|_error| invalid("large property length exceeds usize")),
	}
}

fn contiguous_pages(first: PageNo, count: usize) -> FerrosGrynnResult<Vec<PageNo>> {
	if count == 0 || count > MAX_READ_SCOPE_OPERATIONS {
		return Err(invalid("physical property chain exceeds its bounded read scope"));
	}
	(0..count)
		.map(|offset| {
			let offset = u64::try_from(offset)
				.map_err(|_error| invalid("physical property page offset exceeds u64"))?;
			first
				.get()
				.checked_add(offset)
				.map(PageNo::new)
				.ok_or_else(|| invalid("physical property page number overflows u64"))
		})
		.collect()
}

fn unique_hot_pages(references: &[DiskAtomRef]) -> FerrosGrynnResult<Vec<PageNo>> {
	let mut pages = references
		.iter()
		.map(|reference| hot_slot_address(reference.slot()).map(|(page, _slot)| page))
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	pages.sort_unstable();
	pages.dedup();
	Ok(pages)
}

fn page_index(pages: &[PageNo]) -> RapidHashMap<PageNo, usize> {
	pages.iter().enumerate().map(|(index, page)| (*page, index)).collect()
}

fn validate_node_identity(slot: &NodeHotSlot, source: AtomId) -> FerrosGrynnResult<()> {
	if slot.flags.contains(NodeSlotFlags::TOMBSTONE)
		|| slot.generation != source.generation()
		|| slot.global_atom.is_none()
	{
		return Err(invalid("physical traversal source disagrees with its node hot slot"));
	}
	Ok(())
}

fn validate_edge_identity(slot: &EdgeHotSlot, source: AtomId) -> FerrosGrynnResult<()> {
	if slot.flags.contains(EdgeSlotFlags::TOMBSTONE)
		|| slot.generation != source.generation()
		|| slot.global_atom.is_none()
	{
		return Err(invalid("physical traversal source disagrees with its edge hot slot"));
	}
	Ok(())
}

fn inline_candidate(
	slot: &EdgeHotSlot,
	edge: DiskAtomRef,
	source: DiskAtomRef,
	requested: RequestedDirection,
) -> FerrosGrynnResult<Option<EncodedCandidate>> {
	if edge.kind() != AtomKind::Edge
		|| slot.flags.intersects(EdgeSlotFlags::TOMBSTONE | EdgeSlotFlags::CROSS_LEVEL)
		|| slot.global_atom.is_none()
		|| slot.source_overflow.is_some()
		|| slot.target_overflow.is_some()
		|| slot.source_subgraph_overflow.is_some()
		|| slot.target_subgraph_overflow.is_some()
	{
		return Ok(None);
	}
	let first = slot.source_inline[0];
	let second = match slot.topology {
		EdgeTopologyCode::Directed | EdgeTopologyCode::Bidirectional | EdgeTopologyCode::Loop
			if slot.source_count == 1 && slot.target_count == 1 =>
		{
			slot.target_inline[0]
		}
		EdgeTopologyCode::Undirected if slot.source_count == 2 && slot.target_count == 0 => {
			slot.source_inline[1]
		}
		_ => return Ok(None),
	};
	let (Some(first), Some(second)) = (first, second) else {
		return Err(invalid("binary edge hot slot has an absent inline endpoint"));
	};
	let result = match slot.topology {
		EdgeTopologyCode::Directed | EdgeTopologyCode::Loop
			if matches!(requested, RequestedDirection::Forward) && source == first =>
		{
			Some(EncodedCandidate {
				target: second,
				direction: PathDirection::Forward,
				role: EndpointRole::Target,
				topology: EdgeDirection::Directed,
			})
		}
		EdgeTopologyCode::Directed | EdgeTopologyCode::Loop
			if matches!(requested, RequestedDirection::Backward) && source == second =>
		{
			Some(EncodedCandidate {
				target: first,
				direction: PathDirection::Reverse,
				role: EndpointRole::Source,
				topology: EdgeDirection::Directed,
			})
		}
		EdgeTopologyCode::Bidirectional
			if !matches!(requested, RequestedDirection::Undirected) && source == first =>
		{
			Some(EncodedCandidate {
				target: second,
				direction: PathDirection::Forward,
				role: EndpointRole::Target,
				topology: EdgeDirection::Bidirectional,
			})
		}
		EdgeTopologyCode::Bidirectional
			if !matches!(requested, RequestedDirection::Undirected) && source == second =>
		{
			Some(EncodedCandidate {
				target: first,
				direction: PathDirection::Reverse,
				role: EndpointRole::Source,
				topology: EdgeDirection::Bidirectional,
			})
		}
		EdgeTopologyCode::Undirected
			if matches!(requested, RequestedDirection::Undirected) && source == first =>
		{
			Some(EncodedCandidate {
				target: second,
				direction: PathDirection::Undirected,
				role: EndpointRole::Undirected,
				topology: EdgeDirection::Undirected,
			})
		}
		EdgeTopologyCode::Undirected
			if matches!(requested, RequestedDirection::Undirected) && source == second =>
		{
			Some(EncodedCandidate {
				target: first,
				direction: PathDirection::Undirected,
				role: EndpointRole::Undirected,
				topology: EdgeDirection::Undirected,
			})
		}
		_ => {
			return Err(invalid("node adjacency and binary edge endpoint direction disagree"));
		}
	};
	Ok(result)
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

//! Construction, WAL publication, and snapshot reopen of graph images.

use std::path::Path;

use super::{GraphCheckpointAuthority, GraphFileSet};
use crate::{
	datastore::{DatabaseRoot, GraphRoot},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{GraphId, GraphRef},
	storage::{
		file_registry::FileRegistry,
		graph_index::{BuiltPrimaryIndex, SortedIndexRecords},
		graph_pages::{
			BuiltGraphImage,
			GraphArenaBindings,
			build_graph_image,
			build_import_graph_image,
			open_graph_image,
		},
		graph_superblock::GraphSuperblock,
		image_io::{read_pages, read_pages_cached, stage_pages},
		metadata_cow::select_checkpoint_page,
		page::{AlignedPage, PageLsn, PageNo},
		types::{PageKey, StorageSnapshot},
		wal::{FrameWal, TxHandle},
	},
	values::storage::{DecodeContext, EncodeContext},
};

/// Builds a complete graph generation without publishing it.
pub(in crate::storage) fn build_durable_graph_image(
	database: &DatabaseRoot,
	graph_id: GraphId,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<BuiltGraphImage> {
	build_graph_image(database, graph_id, context, PageLsn::ZERO)
}

/// Builds graph payload with bounded membership input for atomic import.
pub(in crate::storage) fn build_import_durable_graph_image(
	database: &DatabaseRoot,
	graph_id: GraphId,
	context: &mut EncodeContext,
	scratch_root: &Path,
) -> FerrosGrynnResult<BuiltGraphImage<SortedIndexRecords>> {
	build_import_graph_image(database, graph_id, context, PageLsn::ZERO, scratch_root)
}

/// Stages all graph files and publishes the logical superblock last.
pub(in crate::storage) fn stage_durable_graph_image(
	tx: &mut TxHandle,
	files: GraphFileSet,
	image: &BuiltGraphImage,
) -> FerrosGrynnResult<()> {
	validate_image(files, image)?;
	let space = files.space();
	stage_pages(tx, space, files.nodes, &image.nodes)?;
	stage_pages(tx, space, files.edges, &image.edges)?;
	stage_pages(tx, space, files.adjacency, &image.adjacency)?;
	stage_pages(tx, space, files.cross_level_adjacency, &image.cross_level_adjacency)?;
	stage_pages(tx, space, files.overlays, &image.overlays)?;
	stage_pages(tx, space, files.subgraphs, &image.subgraphs)?;
	stage_pages(tx, space, files.labels, &image.labels)?;
	stage_pages(tx, space, files.membership, &image.membership.pages)?;
	tx.stage_page(
		PageKey::new(space, files.superblock, PageNo::new(0)),
		&image.superblock.encode()?,
	)
}

/// Stages graph metadata only when its logical contents changed.
pub(in crate::storage) fn stage_graph_superblock_if_changed(
	tx: &mut TxHandle,
	files: GraphFileSet,
	previous: GraphSuperblock,
	next: GraphSuperblock,
) -> FerrosGrynnResult<bool> {
	if previous.same_logical_metadata(next) {
		return Ok(false);
	}
	tx.stage_page(PageKey::new(files.space(), files.superblock, PageNo::new(0)), &next.encode()?)?;
	Ok(true)
}

/// Reopens one graph through WAL-first snapshot resolution.
#[allow(
	clippy::too_many_arguments,
	reason = "the reopen trust boundary keeps all exact bindings explicit"
)]
pub(in crate::storage) fn open_durable_graph_image(
	wal: &FrameWal,
	registry: &FileRegistry,
	engine: Option<&crate::storage::hybrid_engine::HybridReadEngine>,
	snapshot: StorageSnapshot,
	authority: GraphCheckpointAuthority,
	files: GraphFileSet,
	reference: GraphRef,
	arena: GraphArenaBindings,
	database: &DatabaseRoot,
	context: &DecodeContext<'_>,
) -> FerrosGrynnResult<(GraphRoot, GraphSuperblock)> {
	let superblock = read_graph_superblock(wal, registry, snapshot, authority, files)?;
	if superblock.graph_id != reference.id() || reference.id() != files.graph_id {
		return Err(invalid("graph reference, file set, and superblock identities disagree"));
	}
	let space = files.space();
	let load = |file_id, page_count| match engine {
		Some(engine) => read_pages_cached(
			wal,
			registry,
			engine,
			snapshot,
			authority.generation,
			authority.backfilled_lsn,
			space,
			file_id,
			page_count,
		),
		None => read_pages(wal, registry, snapshot, space, file_id, page_count),
	};
	let membership = read_index(
		wal,
		registry,
		engine,
		snapshot,
		authority,
		space,
		files.membership,
		files.membership_root,
	)?;
	let image = BuiltGraphImage {
		reference,
		superblock,
		nodes: load(files.nodes, superblock.node_hot_pages)?,
		edges: load(files.edges, superblock.edge_hot_pages)?,
		adjacency: load(files.adjacency, superblock.adjacency_overflow_pages)?,
		cross_level_adjacency: load(
			files.cross_level_adjacency,
			superblock.cross_level_adjacency_overflow_pages,
		)?,
		overlays: load(files.overlays, superblock.overlays_pages)?,
		subgraphs: load(files.subgraphs, superblock.subgraphs_pages)?,
		labels: load(files.labels, superblock.labels_pages)?,
		membership,
		arena,
	};
	open_graph_image(image, database, context).map(|graph| (graph, superblock))
}

/// Reads a rooted index using the full snapshot-visible file length.
fn read_index(
	wal: &FrameWal,
	registry: &FileRegistry,
	engine: Option<&crate::storage::hybrid_engine::HybridReadEngine>,
	snapshot: StorageSnapshot,
	authority: GraphCheckpointAuthority,
	space: crate::datastore::DataSpaceId,
	file: crate::storage::types::DataFileId,
	root: PageNo,
) -> FerrosGrynnResult<BuiltPrimaryIndex> {
	let home_pages = registry.resolve_file(space, file)?.file()?.page_count();
	let page_count =
		home_pages.max(wal.visible_file_page_count(snapshot, space, file).unwrap_or(0));
	if page_count == 0 || root.get() >= page_count {
		return Err(invalid("graph membership root exceeds its snapshot-visible file"));
	}
	let pages = match engine {
		Some(engine) => read_pages_cached(
			wal,
			registry,
			engine,
			snapshot,
			authority.generation,
			authority.backfilled_lsn,
			space,
			file,
			page_count,
		)?,
		None => read_pages(wal, registry, snapshot, space, file, page_count)?,
	};
	Ok(BuiltPrimaryIndex {
		root,
		pages,
	})
}

/// Resolves graph metadata from a visible WAL frame or authorized COW home.
fn read_graph_superblock(
	wal: &FrameWal,
	registry: &FileRegistry,
	snapshot: StorageSnapshot,
	authority: GraphCheckpointAuthority,
	files: GraphFileSet,
) -> FerrosGrynnResult<GraphSuperblock> {
	let key = PageKey::new(files.space(), files.superblock, PageNo::new(0));
	let visible = wal.visible_frames(&[key], snapshot);
	if let Some(frame) = visible.frames()[0] {
		let mut page = AlignedPage::zeroed();
		visible.read_frame(frame, &mut page)?;
		let value = GraphSuperblock::decode(&page)?;
		if !authority.accepts_logical(value) {
			return Err(invalid("WAL GraphSuperblock checkpoint tuple exceeds authority"));
		}
		return Ok(value);
	}
	let path = registry.resolve_file(files.space(), files.superblock)?.path().to_path_buf();
	select_checkpoint_page(
		&path,
		GraphSuperblock::decode,
		|value| authority.accepts_checkpoint(*value),
		|value| (value.checkpoint_generation, value.backfilled_lsn, value.checkpoint_record_lsn),
		"GraphSuperblock",
	)
	.map(|(value, _page)| value)
}

/// Checks every count and rooted index before WAL publication.
fn validate_image(files: GraphFileSet, image: &BuiltGraphImage) -> FerrosGrynnResult<()> {
	if files.graph_id != image.reference.id() || files.graph_id != image.superblock.graph_id {
		return Err(invalid("graph image and physical file set identities disagree"));
	}
	image.superblock.encode()?;
	for (actual, expected, family) in [
		(image.nodes.len(), image.superblock.node_hot_pages, "node-hot"),
		(image.edges.len(), image.superblock.edge_hot_pages, "edge-hot"),
		(image.adjacency.len(), image.superblock.adjacency_overflow_pages, "adjacency"),
		(
			image.cross_level_adjacency.len(),
			image.superblock.cross_level_adjacency_overflow_pages,
			"cross-level adjacency",
		),
		(image.overlays.len(), image.superblock.overlays_pages, "overlays"),
		(image.subgraphs.len(), image.superblock.subgraphs_pages, "subgraphs"),
		(image.labels.len(), image.superblock.labels_pages, "labels"),
	] {
		if u64::try_from(actual).ok() != Some(expected) {
			return Err(invalid(format!("{family} page ceiling disagrees with graph image")));
		}
	}
	let index_pages = u64::try_from(image.membership.pages.len())
		.map_err(|_error| invalid("graph membership page count exceeds u64"))?;
	if index_pages == 0 || image.membership.root != files.membership_root {
		return Err(invalid("graph membership root disagrees with its physical binding"));
	}
	if image.membership.root.get() >= index_pages {
		return Err(invalid("graph membership root exceeds its image"));
	}
	Ok(())
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

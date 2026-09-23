//! Exact catalog bindings used by graph-local durable images.

use crate::{
	datastore::DataSpaceId,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::GraphId,
	storage::{
		catalog::FileCatalog,
		page::PageNo,
		types::{DataFileId, DataFileKind},
	},
};

/// Complete physical file set for one graph data space.
#[derive(Debug, Clone, Copy)]
pub(in crate::storage) struct GraphFileSet {
	/// Owning graph identity.
	pub(in crate::storage) graph_id: GraphId,
	/// Checkpoint-COW graph superblock.
	pub(in crate::storage) superblock: DataFileId,
	/// Fixed node-occurrence rows.
	pub(in crate::storage) nodes: DataFileId,
	/// Fixed edge-occurrence rows.
	pub(in crate::storage) edges: DataFileId,
	/// Same-level adjacency overflow.
	pub(in crate::storage) adjacency: DataFileId,
	/// Qualified cross-level adjacency overflow.
	pub(in crate::storage) cross_level_adjacency: DataFileId,
	/// Sparse graph-local property overlays.
	pub(in crate::storage) overlays: DataFileId,
	/// Direct nested-graph directory.
	pub(in crate::storage) subgraphs: DataFileId,
	/// Node/edge label dictionary.
	pub(in crate::storage) labels: DataFileId,
	/// `GlobalAtomId -> DiskAtomRef` graph membership index.
	pub(in crate::storage) membership: DataFileId,
	/// Root page of the graph membership index.
	pub(in crate::storage) membership_root: PageNo,
}

impl GraphFileSet {
	/// Resolves singleton files and validates the explicit membership binding.
	pub(in crate::storage) fn from_catalog(
		catalog: &FileCatalog,
		graph_id: GraphId,
		membership: DataFileId,
		membership_root: PageNo,
	) -> FerrosGrynnResult<Self> {
		let space = DataSpaceId::Graph(graph_id);
		validate_index_binding(catalog, space, membership, membership_root, "graph membership")?;
		Ok(Self {
			graph_id,
			superblock: singleton(catalog, space, DataFileKind::GraphSuperblock)?,
			nodes: singleton(catalog, space, DataFileKind::NodesHot)?,
			edges: singleton(catalog, space, DataFileKind::EdgesHot)?,
			adjacency: singleton(catalog, space, DataFileKind::AdjacencyOverflow)?,
			cross_level_adjacency: singleton(
				catalog,
				space,
				DataFileKind::CrossLevelAdjacencyOverflow,
			)?,
			overlays: singleton(catalog, space, DataFileKind::Overlays)?,
			subgraphs: singleton(catalog, space, DataFileKind::Subgraphs)?,
			labels: singleton(catalog, space, DataFileKind::Labels)?,
			membership,
			membership_root,
		})
	}

	/// Returns the graph address space carried by every binding.
	pub(in crate::storage) const fn space(self) -> DataSpaceId {
		DataSpaceId::Graph(self.graph_id)
	}

	/// Returns WAL-backfilled files in the graph generation.
	pub(in crate::storage) const fn regular_files(self) -> [DataFileId; 8] {
		[
			self.nodes,
			self.edges,
			self.adjacency,
			self.cross_level_adjacency,
			self.overlays,
			self.subgraphs,
			self.labels,
			self.membership,
		]
	}
}

/// Physical binding for database-global `memberships.fgidx`.
#[derive(Debug, Clone, Copy)]
pub(in crate::storage) struct ReverseMembershipFile {
	/// Physical index file.
	pub(in crate::storage) file: DataFileId,
	/// Root page of the reverse membership B+tree.
	pub(in crate::storage) root: PageNo,
}

impl ReverseMembershipFile {
	/// Validates an explicit database-space index binding.
	pub(in crate::storage) fn from_catalog(
		catalog: &FileCatalog,
		file: DataFileId,
		root: PageNo,
	) -> FerrosGrynnResult<Self> {
		validate_index_binding(catalog, DataSpaceId::Database, file, root, "reverse membership")?;
		Ok(Self {
			file,
			root,
		})
	}
}

/// Resolves exactly one singleton kind inside one data space.
fn singleton(
	catalog: &FileCatalog,
	space: DataSpaceId,
	kind: DataFileKind,
) -> FerrosGrynnResult<DataFileId> {
	let mut matches = catalog
		.files
		.iter()
		.filter(|entry| entry.space_id == space && entry.file_id.kind() == Some(kind));
	let entry = matches
		.next()
		.ok_or_else(|| invalid(format!("data space {} has no {kind:?} file", space.raw())))?;
	if matches.next().is_some() {
		return Err(invalid(format!("data space {} has multiple {kind:?} files", space.raw())));
	}
	Ok(entry.file_id)
}

/// Confirms that an explicitly cataloged file can host a rooted index.
fn validate_index_binding(
	catalog: &FileCatalog,
	space: DataSpaceId,
	file: DataFileId,
	root: PageNo,
	name: &str,
) -> FerrosGrynnResult<()> {
	if file.kind() != Some(DataFileKind::Index) || root == PageNo::NULL {
		return Err(invalid(format!("{name} has an invalid physical binding")));
	}
	if !catalog.files.iter().any(|entry| entry.space_id == space && entry.file_id == file) {
		return Err(invalid(format!("{name} file is absent from FileCatalog")));
	}
	Ok(())
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

//! Sparse physical plans for Base DML and stable graph-local changes.

use std::{collections::BTreeMap, sync::Arc};

use super::{LocalState, LocalStore, layout::invalid};
use crate::{
	datastore::{
		ClassId,
		ClassIndexKind,
		DataSpaceId,
		DatabaseRoot,
		GlobalAtomId,
		PublicationDelta,
	},
	gql_status::FerrosGrynnResult,
	graph::AtomKind,
	storage::{
		atom_store_superblock::AtomStoreSuperblock,
		graph_pages::GraphArenaBindings,
		graph_superblock::GraphSuperblock,
		image_io::{read_page_cached, visible_file_page_count},
		page::AlignedPage,
		types::{DataFileId, DataFileKind, PageKey},
		wal::TxHandle,
	},
	values::storage::EncodeContext,
};

/// Heap-stable changed pages retained through the WAL publication barrier.
type PointPages = Vec<(PageKey, Box<AlignedPage>)>;

/// Complete fallible preparation; staging never builds a full generation.
pub(super) struct PointUpdatePlan {
	/// Sparse pages keyed by their existing physical file identities.
	pages: PointPages,
	/// Logical ceilings with the latest database-authorized checkpoint tuple.
	pub(super) atom_superblock: AtomStoreSuperblock,
	/// A complete RAM statistics root replaces the old lazy physical owner.
	pub(super) replaces_statistics: bool,
	/// Existing graph metadata is replaced only after the common WAL commit.
	pub(super) graph_superblocks: BTreeMap<crate::graph::GraphId, GraphSuperblock>,
	/// Prepared immutable owners also become the durable catalog authority.
	pub(super) graph_arenas: BTreeMap<crate::graph::GraphId, GraphArenaBindings>,
}

impl PointUpdatePlan {
	/// Requires exhaustive replay slots and supported occurrence transitions.
	///
	/// # Errors
	/// Rejects an invalid replay slot rather than masking it as a fallback.
	pub(super) fn eligible(
		before: &DatabaseRoot,
		after: &DatabaseRoot,
		delta: &PublicationDelta,
	) -> FerrosGrynnResult<bool> {
		let dictionary_changed = !Arc::ptr_eq(&before.dictionary, &after.dictionary);
		let stable_dictionary = !dictionary_changed
			|| (before.dictionary.len() <= after.dictionary.len()
				&& before
					.dictionary
					.iter()
					.all(|(id, name)| after.dictionary.resolve(id) == Some(name)));
		if (delta.global_slots.is_empty()
			&& delta.overlays.is_empty()
			&& delta.incidence.is_empty()
			&& !dictionary_changed)
			|| !((Arc::ptr_eq(&before.schema, &after.schema)
				|| after.schema.is_endpoint_extension_of(&before.schema, after.commit_ts))
				&& stable_dictionary
				&& Arc::ptr_eq(&before.resident_geometry, &after.resident_geometry)
				&& Arc::ptr_eq(&before.reachability_artifacts, &after.reachability_artifacts)
				&& Arc::ptr_eq(&before.neighbourhood_artifacts, &after.neighbourhood_artifacts)
				&& Arc::ptr_eq(&before.path_artifacts, &after.path_artifacts)
				&& Arc::ptr_eq(&before.tensor_artifacts, &after.tensor_artifacts)
				&& before.graphs.len() == after.graphs.len())
		{
			return Ok(false);
		}
		if !super::point_graph::eligible(before, after, delta) {
			return Ok(false);
		}
		for slot in &delta.global_slots {
			let old = before.atoms.atom_by_slot(*slot)?;
			let new = after.atoms.atom_by_slot(*slot)?;
			for atom in old.into_iter().chain(new) {
				// shortcut: specialized indexes retain their verified publisher
				// until their sparse delta codecs are implemented.
				if !supports_point_indexes(after, atom.class_id()) {
					return Ok(false);
				}
			}
			// Every side of an attached lifecycle must reach the same incidence
			// publisher; a membership key alone cannot update its Hot slot.
			let stable_identity = old
				.zip(new)
				.is_some_and(|(old, new)| old.id() == new.id() && old.class_id() == new.class_id());
			if !stable_identity {
				for atom in old.into_iter().chain(new) {
					if !lifecycle_memberships_complete(before, after, delta, atom.id()) {
						return Ok(false);
					}
				}
			}
		}
		Ok(true)
	}

	/// Encodes only changed atoms, append-only property pages and eager roots.
	///
	/// # Errors
	/// Propagates source identity, page codec, count and I/O failures before
	/// WAL.
	pub(super) fn build(
		store: &LocalStore,
		state: &LocalState,
		before: &DatabaseRoot,
		after: &DatabaseRoot,
		delta: &PublicationDelta,
		labels: &mut super::point_namespace::LabelPlan,
		context: &mut EncodeContext,
	) -> FerrosGrynnResult<Self> {
		let base = super::point_base::BaseUpdatePlan::build(
			store,
			state,
			before,
			after,
			delta,
			context,
			&mut labels.budget,
		)?;
		let mut pages = base.pages;
		let mut replaces_statistics = base.replaces_statistics;
		let atom_superblock = base.superblock;
		let graph = super::point_graph::GraphUpdatePlan::build(
			store, state, before, after, delta, labels, context,
		)?;
		pages.extend(graph.pages);
		replaces_statistics |= super::point_statistics::append_statistics_pages(
			store, state, before, after, delta, labels, &mut pages,
		)?;
		Ok(Self {
			pages,
			atom_superblock,
			replaces_statistics,
			graph_superblocks: graph.superblocks,
			graph_arenas: graph.arenas,
		})
	}

	/// Adds the prepared sparse image to the same source/history WAL barrier.
	pub(super) fn stage(&self, tx: &mut TxHandle) -> FerrosGrynnResult<()> {
		for (key, page) in &self.pages {
			tx.stage_page(*key, page)?;
		}
		Ok(())
	}
}

/// Every live Base side must reach the same membership and Hot-slot publisher.
fn lifecycle_memberships_complete(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	delta: &PublicationDelta,
	global: GlobalAtomId,
) -> bool {
	for root in [before, after] {
		if let Some(members) = root.memberships(global)
			&& members.iter().any(|(graph, local)| {
				!matches!(global.kind(), Some(AtomKind::Node | AtomKind::Edge))
					|| !delta.memberships.contains(&(*graph, global))
					|| !delta.incidence.contains(&(*graph, *local))
			}) {
			return false;
		}
	}
	true
}

/// Specialized codecs retain their verified full-generation publisher.
pub(super) fn supports_point_indexes(root: &DatabaseRoot, class: ClassId) -> bool {
	root.schema.class_by_id(class).is_some_and(|class| {
		class.indexes().all(|index| {
			matches!(
				index.kind(),
				ClassIndexKind::Standard | ClassIndexKind::Unique | ClassIndexKind::Count
			)
		})
	})
}

impl LocalStore {
	/// Completes the actual publication root before encoders borrow it. A full
	/// fallback drops the old physical binding and cannot publish lazy maps.
	pub(crate) fn prepare_publication_statistics(
		&self,
		database: &mut DatabaseRoot,
		previous: Option<&DatabaseRoot>,
		delta: Option<&PublicationDelta>,
		catalog_only: bool,
	) -> FerrosGrynnResult<()> {
		let sparse = previous
			.zip(delta)
			.map(|(before, delta)| PointUpdatePlan::eligible(before, database, delta))
			.transpose()?
			.unwrap_or(false);
		let lifecycle = previous
			.zip(delta)
			.map(|(before, delta)| super::point_base::has_lifecycle(before, database, delta))
			.transpose()?
			.unwrap_or(false);
		if !catalog_only && (!sparse || lifecycle) && database.statistics.auxiliary_deferred() {
			self.hydrate_statistics_at(
				database,
				previous.map_or(database.commit_ts, |root| root.commit_ts),
			)?;
		}
		Ok(())
	}

	/// Reads one current page under the publication lock and checkpoint fence.
	///
	/// # Errors
	/// Propagates registry, WAL/Home resolution and page-read failures.
	pub(super) fn read_update_page(
		&self,
		state: &LocalState,
		key: PageKey,
	) -> FerrosGrynnResult<Box<AlignedPage>> {
		let mut page = Box::new(AlignedPage::zeroed());
		self.read_update_page_into(state, key, &mut page)?;
		Ok(page)
	}

	/// Shares the fenced current-page reader with scalar leaf preparation.
	///
	/// # Errors
	/// Propagates registry, WAL/Home resolution and page-read failures.
	pub(super) fn read_update_page_into(
		&self,
		state: &LocalState,
		key: PageKey,
		page: &mut AlignedPage,
	) -> FerrosGrynnResult<()> {
		let snapshot = self.wal.snapshot();
		read_page_cached(
			&self.wal,
			&state.registry,
			&self.engine,
			snapshot,
			state.database_superblock.generation,
			snapshot.visible_lsn(),
			key,
			page,
		)
	}

	/// Includes WAL-only appends when choosing the next absolute page number.
	///
	/// # Errors
	/// Propagates absent bindings and visible file-length resolution failures.
	pub(super) fn visible_pages(
		&self,
		state: &LocalState,
		space: DataSpaceId,
		file: DataFileId,
	) -> FerrosGrynnResult<u64> {
		visible_file_page_count(&self.wal, &state.registry, self.wal.snapshot(), space, file)
	}

	/// Resolves an existing singleton without allocating a new file identity.
	pub(super) fn statistics_file(
		state: &LocalState,
		space: DataSpaceId,
		kind: DataFileKind,
	) -> FerrosGrynnResult<DataFileId> {
		state
			.file_catalog
			.files
			.iter()
			.find(|entry| entry.space_id == space && entry.file_id.kind() == Some(kind))
			.map(|entry| entry.file_id)
			.ok_or_else(|| invalid("point update statistics file is absent"))
	}
}

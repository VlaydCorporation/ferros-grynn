//! B+tree images for forward and reverse graph membership resolution.

use super::{
	graph_index::{BuiltPrimaryIndex, PrimaryIndexEntry, build_primary_index},
	page::PageLsn,
};
use crate::{
	datastore::{AtomStoreRoot, GlobalAtomId, GraphRoot},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomKind, GraphId},
	values::storage::DiskAtomRef,
};

/// Builds `membership.fgidx`: one unique global identity per graph occurrence.
pub(super) fn build_graph_membership_index(
	graph: &GraphRoot,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<BuiltPrimaryIndex> {
	let mut entries = graph
		.occurrences()
		.map(|occurrence| {
			Ok(PrimaryIndexEntry {
				key: occurrence.global_id().raw().to_be_bytes().to_vec(),
				atom: disk_ref(occurrence.local_id())?,
			})
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	entries.sort_unstable_by(|left, right| left.key.cmp(&right.key));
	build_primary_index(&entries, page_lsn)
}

/// Builds database `memberships.fgidx` in `(global, graph)` order.
pub(super) fn build_reverse_membership_index(
	atoms: &AtomStoreRoot,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<BuiltPrimaryIndex> {
	let mut entries = Vec::new();
	for atom in atoms.atoms() {
		let Some(memberships) = atoms.memberships(atom.id()) else {
			continue;
		};
		for (graph, local) in memberships {
			entries.push(PrimaryIndexEntry {
				key: reverse_key(atom.id(), *graph),
				atom: disk_ref(*local)?,
			});
		}
	}
	build_primary_index(&entries, page_lsn)
}

/// Encodes the exact reverse-membership ordering key.
pub(super) fn reverse_key(global: GlobalAtomId, graph: GraphId) -> Vec<u8> {
	let mut key = Vec::with_capacity(16);
	key.extend_from_slice(&global.raw().to_be_bytes());
	key.extend_from_slice(&graph.get().to_be_bytes());
	key
}

fn disk_ref(id: crate::graph::AtomId) -> FerrosGrynnResult<DiskAtomRef> {
	let kind = id
		.kind()
		.ok_or_else(|| FerrosGrynnError::invalid_storage_file("membership contains a null atom"))?;
	if !matches!(kind, AtomKind::Node | AtomKind::Edge) {
		return Err(FerrosGrynnError::invalid_storage_file(
			"membership contains an unknown atom kind",
		));
	}
	DiskAtomRef::from_parts(kind, id.slot())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::graph::{AtomId, AtomKind};

	#[test]
	fn reverse_key_groups_graphs_under_full_generation_aware_global_id() -> FerrosGrynnResult<()> {
		let global = GlobalAtomId::from_raw(AtomId::from_parts(AtomKind::Node, 8, 3)?.raw());
		let first = reverse_key(global, GraphId::new(2).unwrap());
		let second = reverse_key(global, GraphId::new(9).unwrap());
		assert_eq!(&first[..8], &global.raw().to_be_bytes());
		assert!(first < second);
		Ok(())
	}
}

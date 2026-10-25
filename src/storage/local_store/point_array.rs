//! Shared checked source merge for immutable graph membership arrays.

use std::collections::BTreeSet;

use super::{LocalState, LocalStore, layout::invalid};
use crate::{
	datastore::{DataSpaceId, GraphRoot},
	gql_status::FerrosGrynnResult,
	graph::{AtomId, AtomKind},
	storage::{
		graph_index::{LabelIndexWriter, StreamedIndex, visit_label_index_pages},
		page::{AlignedPage, PageLsn, PageNo},
		scoped_index_image::local_address,
		types::{DataFileId, PageKey},
	},
	values::storage::DiskAtomRef,
};

/// Merges only explicit insertions; the existing population stays ordered.
///
/// # Errors
/// Rejects inconsistent source/final membership and propagates codec/I/O
/// errors.
pub(super) fn stream_membership_array(
	store: &LocalStore,
	state: &LocalState,
	graphs: (&GraphRoot, &GraphRoot),
	previous: Option<DataFileId>,
	kind: AtomKind,
	old_atoms: Option<&BTreeSet<AtomId>>,
	atoms: &BTreeSet<AtomId>,
	insertions: &[AtomId],
	write_page: impl FnMut(PageNo, &AlignedPage) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<StreamedIndex> {
	let mut writer = LabelIndexWriter::new(atoms.len(), kind, PageLsn::ZERO, write_page);
	let mut next = 0;
	visit_source(store, state, graphs.0, previous, kind, old_atoms, |_local, address| {
		let slot = usize::try_from(address.slot())
			.map_err(|_error| invalid("membership array slot exceeds usize"))?;
		let retained = graphs
			.1
			.slot_generation(kind, slot)
			.map(|generation| AtomId::from_parts(kind, address.slot(), generation))
			.transpose()?
			.is_some_and(|local| atoms.contains(&local) && graphs.1.occurrence(local).is_some());
		while let Some(inserted) =
			insertions.get(next).filter(|inserted| inserted.slot() < address.slot())
		{
			if !atoms.contains(inserted) {
				return Err(invalid("point label insertion disagrees with final membership"));
			}
			writer.push(local_address(*inserted)?)?;
			next += 1;
		}
		if insertions.get(next).is_some_and(|inserted| inserted.slot() == address.slot()) {
			if retained || !atoms.contains(&insertions[next]) {
				return Err(invalid("membership array insertion duplicates a retained slot"));
			}
			writer.push(address)?;
			next += 1;
		}
		// Disk postings omit generations; a reused slot can retain its address.
		if retained {
			writer.push(address)?;
		}
		Ok(())
	})?;
	for inserted in &insertions[next..] {
		if !atoms.contains(inserted) {
			return Err(invalid("point label insertion disagrees with final membership"));
		}
		writer.push(local_address(*inserted)?)?;
	}
	writer.finish()
}

/// Validates every source posting, including a removed tail after output ends.
///
/// # Errors
/// Rejects source presence, generation, population and canonical-page errors.
pub(super) fn visit_source(
	store: &LocalStore,
	state: &LocalState,
	source: &GraphRoot,
	file: Option<DataFileId>,
	kind: AtomKind,
	expected: Option<&BTreeSet<AtomId>>,
	mut visit: impl FnMut(AtomId, DiskAtomRef) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	let (Some(file), Some(expected)) = (file, expected) else {
		return if file.is_none() && expected.is_none() {
			Ok(())
		} else {
			Err(invalid("point label source presence disagrees"))
		};
	};
	let graph = source.reference().id();
	let mut count = 0_usize;
	visit_label_index_pages::<DiskAtomRef>(
		|number, output| {
			store.read_update_page_into(
				state,
				PageKey::new(DataSpaceId::Graph(graph), file, number),
				output,
			)
		},
		kind,
		|address| {
			let slot = usize::try_from(address.slot())
				.map_err(|_error| invalid("point label source slot exceeds usize"))?;
			let generation = source
				.slot_generation(kind, slot)
				.ok_or_else(|| invalid("point label source generation is absent"))?;
			let local = AtomId::from_parts(kind, address.slot(), generation)?;
			if !expected.contains(&local) || source.occurrence(local).is_none() {
				return Err(invalid("point label source posting disagrees"));
			}
			count = count
				.checked_add(1)
				.ok_or_else(|| invalid("point label source count overflows"))?;
			visit(local, address)
		},
	)?;
	if count != expected.len() {
		return Err(invalid("point label source omits postings"));
	}
	Ok(())
}

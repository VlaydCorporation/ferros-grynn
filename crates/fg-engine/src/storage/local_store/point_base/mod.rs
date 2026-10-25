//! Base-owned Hot, property append and allocator publication preparation.

use std::collections::BTreeMap;

use super::{LocalState, LocalStore, layout::invalid, point_namespace::PublicationBudget};
use crate::{
	datastore::{BaseAtom, DataSpaceId, DatabaseRoot, GlobalAtomId, PublicationDelta},
	gql_status::FerrosGrynnResult,
	graph::{AtomId, AtomKind},
	storage::{
		atom_page::{ATOM_SLOTS_PER_PAGE, AtomHotPage, AtomHotSlot, PropertyRef, atom_location},
		atom_store_image::PropertyStoreBuilder,
		atom_store_superblock::AtomStoreSuperblock,
		page::{AlignedPage, PAGE_SIZE, PageLsn, PageNo},
		property_page::encode_property_map_bounded,
		types::PageKey,
	},
	values::storage::EncodeContext,
};

/// Sequential property preparation shares final-frame and WAL admission.
mod properties;
use properties::BaseProperties;

/// Base frames are composed with graph and statistics frames before one WAL.
pub(super) struct BaseUpdatePlan {
	/// Existing file identities and immutable property prefixes are retained.
	pub(super) pages: Vec<(PageKey, Box<AlignedPage>)>,
	/// Final allocator ceilings and database-authorized checkpoint metadata.
	pub(super) superblock: AtomStoreSuperblock,
	/// Lifecycle publication replaces deferred statistics with the final root.
	pub(super) replaces_statistics: bool,
}

impl BaseUpdatePlan {
	/// Prepares only final replay slots and their append-only property records.
	///
	/// # Errors
	/// Propagates source identity, codec, allocator, count and I/O failures.
	pub(super) fn build(
		store: &LocalStore,
		state: &LocalState,
		before: &DatabaseRoot,
		after: &DatabaseRoot,
		delta: &PublicationDelta,
		context: &mut EncodeContext,
		budget: &mut PublicationBudget,
	) -> FerrosGrynnResult<Self> {
		let files = state.bindings.atoms;
		let start = if delta.global_slots.is_empty() {
			state.atom_superblock.properties_pages
		} else {
			store.visible_pages(state, DataSpaceId::Database, files.properties)?
		};
		let mut properties = BaseProperties::new(start, context, budget);
		let mut hot = BTreeMap::<PageNo, AtomHotPage>::new();
		let replaces_statistics = has_lifecycle(before, after, delta)?;
		let mut atom_superblock = state.atom_superblock;
		for slot in &delta.global_slots {
			let old = before.atoms.atom_by_slot(*slot)?;
			let new = after.atoms.atom_by_slot(*slot)?;
			let (number, within) = atom_location(*slot);
			if !hot.contains_key(&number) {
				properties.budget.tree_entry::<(PageNo, AtomHotPage)>(hot.len())?;
				properties
					.budget
					.retain(PAGE_SIZE + 3 * size_of::<(PageKey, Box<AlignedPage>)>())?;
				properties.budget.stage_pages(1)?;
				properties.check_scratch(4096)?;
				let page = if number.get() < state.atom_superblock.atoms_hot_pages {
					AtomHotPage::from_page(
						store.read_update_page(
							state,
							PageKey::new(DataSpaceId::Database, files.atoms_hot, number),
						)?,
						number,
					)?
				} else {
					AtomHotPage::new(number, PageLsn::ZERO)?
				};
				hot.insert(number, page);
			}
			let page =
				hot.get_mut(&number).ok_or_else(|| invalid("point update hot page is absent"))?;
			write_slot(page, within, *slot, before, after, &mut properties)?;
			update_population(&mut atom_superblock, old, false)?;
			update_population(&mut atom_superblock, new, true)?;
		}
		let header_count = usize::from(!delta.global_slots.is_empty());
		if header_count != 0 {
			properties.budget.retain(PAGE_SIZE + 3 * size_of::<(PageKey, Box<AlignedPage>)>())?;
			properties.budget.stage_pages(1)?;
		}
		properties.check_scratch(0)?;
		let count = hot
			.len()
			.checked_add(properties.page_count())
			.and_then(|count| count.checked_add(header_count))
			.ok_or_else(|| invalid("Base final page count overflows usize"))?;
		let mut pages = Vec::new();
		pages.try_reserve_exact(count).map_err(|source| {
			invalid("Base page key allocation failed").with_technical_source(source)
		})?;
		for (number, page) in hot {
			pages.push((
				PageKey::new(DataSpaceId::Database, files.atoms_hot, number),
				page.finish(),
			));
		}
		let property_pages = properties.finish()?;
		let end = start
			.checked_add(
				u64::try_from(property_pages.len())
					.map_err(|_error| invalid("property append length exceeds u64"))?,
			)
			.ok_or_else(|| invalid("property append ceiling overflows"))?;
		for (index, page) in property_pages.into_iter().enumerate() {
			let number = start
				.checked_add(index as u64)
				.ok_or_else(|| invalid("property append address overflows"))?;
			pages.push((
				PageKey::new(DataSpaceId::Database, files.properties, PageNo::new(number)),
				Box::new(page),
			));
		}
		// A failed checkpoint may have written subordinate metadata ahead of
		// authority. Only the database commit point authorizes this tuple.
		if !delta.global_slots.is_empty() {
			atom_superblock.page_lsn = PageLsn::ZERO;
			atom_superblock.checkpoint_generation = state.database_superblock.generation;
			atom_superblock.backfilled_lsn = state.database_superblock.checkpoint_lsn;
			atom_superblock.checkpoint_record_lsn = state.database_superblock.checkpoint_record_lsn;
			atom_superblock.properties_pages = end;
			atom_superblock.atom_capacity = u64::try_from(after.atoms.slot_capacity())
				.map_err(|_error| invalid("point atom capacity exceeds u64"))?;
			atom_superblock.atoms_hot_pages =
				atom_superblock.atom_capacity.div_ceil(ATOM_SLOTS_PER_PAGE);
			pages.push((
				PageKey::new(DataSpaceId::Database, files.superblock, PageNo::new(0)),
				Box::new(atom_superblock.encode()?),
			));
		}
		Ok(Self {
			pages,
			superblock: atom_superblock,
			replaces_statistics,
		})
	}
}

/// Detects allocator changes as well as net live identities.
pub(super) fn has_lifecycle(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	delta: &PublicationDelta,
) -> FerrosGrynnResult<bool> {
	if before.atoms.slot_capacity() != after.atoms.slot_capacity() {
		return Ok(true);
	}
	for slot in &delta.global_slots {
		if before.atoms.atom_by_slot(*slot)?.map(BaseAtom::id)
			!= after.atoms.atom_by_slot(*slot)?.map(BaseAtom::id)
			|| slot_generation(before, *slot)? != slot_generation(after, *slot)?
		{
			return Ok(true);
		}
	}
	Ok(false)
}

/// Validates the source slot before installing its final, coalesced image.
fn write_slot(
	page: &mut AtomHotPage,
	within: u64,
	slot: u64,
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	properties: &mut BaseProperties<'_>,
) -> FerrosGrynnResult<()> {
	let old = before.atoms.atom_by_slot(slot)?;
	let new = after.atoms.atom_by_slot(slot)?;
	let (id, physical) = page.read(within)?;
	let record = match (old, physical) {
		(
			Some(atom),
			AtomHotSlot::Live {
				kind,
				class_id,
				record_id,
				..
			},
		) if id == atom.id() && class_id == atom.class_id() && id.kind() == Some(kind) => record_id,
		(None, AtomHotSlot::Tombstone)
			if id.generation() == slot_generation(before, slot)?.unwrap_or(0) =>
		{
			PropertyRef::None
		}
		_ => return Err(invalid("point source physical identity disagrees with its atom")),
	};
	if let Some(atom) = new {
		let record = if old.is_some_and(|old| old.record_id() == atom.record_id()) {
			record
		} else {
			properties.record(atom.record_id())?
		};
		let value = if atom.properties().is_empty() {
			PropertyRef::None
		} else {
			properties.properties(atom.properties())?
		};
		page.write(
			atom.id(),
			AtomHotSlot::live(
				atom.id().kind().ok_or_else(|| invalid("point atom kind is null"))?,
				atom.class_id(),
				record,
				value,
			)?,
		)?;
	} else {
		let generation = slot_generation(after, slot)?
			.ok_or_else(|| invalid("point tombstone has no allocator generation"))?;
		page.write(
			GlobalAtomId::from_raw(AtomId::from_parts(AtomKind::Node, slot, generation)?.raw()),
			AtomHotSlot::Tombstone,
		)?;
	}
	Ok(())
}

/// Reads allocator generations with the platform conversion checked once.
fn slot_generation(root: &DatabaseRoot, slot: u64) -> FerrosGrynnResult<Option<u32>> {
	let slot = usize::try_from(slot).map_err(|_error| invalid("point slot exceeds usize"))?;
	Ok(root.atoms.slot_generation(slot))
}

/// Applies checked live-kind deltas without recounting the whole AtomStore.
fn update_population(
	superblock: &mut AtomStoreSuperblock,
	atom: Option<&BaseAtom>,
	insert: bool,
) -> FerrosGrynnResult<()> {
	let Some(atom) = atom else {
		return Ok(());
	};
	let count = match atom.id().kind() {
		Some(AtomKind::Node) => &mut superblock.node_count,
		Some(AtomKind::Edge) => &mut superblock.edge_count,
		None => return Err(invalid("point population has null atom kind")),

		_ => {
			return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
				"Unsupported runtime variant at the workspace boundary.",
			));
		}
	};
	*count = if insert {
		count.checked_add(1)
	} else {
		count.checked_sub(1)
	}
	.ok_or_else(|| invalid("point population overflows or underflows"))?;
	Ok(())
}

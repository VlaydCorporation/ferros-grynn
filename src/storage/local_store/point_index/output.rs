//! Retained index frames and their future WAL copies share one publication
//! account.

use std::mem::size_of;

use super::{LocalState, LocalStore, PointPages};
use crate::{
	datastore::DataSpaceId,
	gql_status::FerrosGrynnResult,
	storage::{
		local_store::{layout::invalid, point_namespace::PublicationBudget},
		page::{AlignedPage, PAGE_SIZE},
		types::{DataFileId, PageKey},
	},
};

/// Bounds a stable probe before the leaf codec allocates output frames.
///
/// # Errors
/// Preserves source-ceiling, checked-size and shared-budget failures.
pub(super) fn admit_leaves(
	store: &LocalStore,
	state: &LocalState,
	space: DataSpaceId,
	file: DataFileId,
	mutations: usize,
	budget: &PublicationBudget,
) -> FerrosGrynnResult<()> {
	let source_pages = usize::try_from(store.visible_pages(state, space, file)?)
		.map_err(|_error| invalid("index output page ceiling exceeds usize"))?;
	// Core B+trees start at leaf page zero, including an empty logical tree.
	admit(budget, mutations.min(source_pages))
}

/// Checks prepared frames, vector growth and the future WAL reservation.
///
/// # Errors
/// Rejects the complete logical output peak without advancing either account.
pub(super) fn admit(budget: &PublicationBudget, count: usize) -> FerrosGrynnResult<()> {
	if count == 0 {
		return Ok(());
	}
	let (_, staged) = budget.staging_reservation(count)?;
	let peak = frame_bytes(count)?
		.checked_add(staged)
		.and_then(|bytes| bytes.checked_add(4096))
		.ok_or_else(|| invalid("index output peak bytes overflow usize"))?;
	budget.scratch(peak)
}

/// Charges actual stable output; discarded fallback probes retain no charge.
///
/// # Errors
/// Propagates admission, WAL sizing and destination-allocation failures.
pub(super) fn retain(
	pages: &mut PointPages,
	count: usize,
	budget: &mut PublicationBudget,
) -> FerrosGrynnResult<()> {
	if count == 0 {
		return Ok(());
	}
	admit(budget, count)?;
	budget.retain(frame_bytes(count)?)?;
	budget.stage_pages(count)?;
	reserve_keys(pages, count)
}

/// Keeps moved frames within the already admitted vector-growth allowance.
///
/// # Errors
/// Preserves a destination allocation failure with its technical cause.
pub(super) fn reserve_keys(pages: &mut PointPages, count: usize) -> FerrosGrynnResult<()> {
	pages.try_reserve_exact(count).map_err(|source| {
		invalid("index output page key allocation failed").with_technical_source(source)
	})
}

/// Three slots cover probe backing and old/new destination backing during
/// growth.
///
/// # Errors
/// Rejects a page-count multiplication that exceeds the platform address space.
fn frame_bytes(count: usize) -> FerrosGrynnResult<usize> {
	count
		.checked_mul(PAGE_SIZE + 3 * size_of::<(PageKey, Box<AlignedPage>)>())
		.ok_or_else(|| invalid("index output frame bytes overflow usize"))
}

//! Borrowed page guards and bounded read-epoch lifetime proofs.

use std::sync::atomic::Ordering;

use super::{
	AtomicArenaBufferPool,
	LIFECYCLE_MASK,
	Lifecycle,
	generation,
	lifecycle,
	locator::{FrameLocator, ResolvedFrameHandle},
	pins,
};
use crate::storage::page::{AlignedPage, PAGE_SIZE};

/// Cursor-scoped reclamation guard for borrowed arena page addresses.
///
/// ponytail: one global reader count delays every retired frame; replace it
/// with generation-bucketed epochs only if measured miss-side backpressure
/// makes that extra machinery necessary.
pub struct AtomicReadEpoch<'pool> {
	/// Arena whose retired bytes remain immutable until this guard drops.
	pub(super) pool: &'pool AtomicArenaBufferPool,
}

impl AtomicReadEpoch<'_> {
	/// Borrows a page through one cached locator without per-page pin/drop.
	pub fn get_resolved<'epoch>(
		&'epoch self,
		handle: &ResolvedFrameHandle,
	) -> Option<AtomicEpochPage<'epoch>> {
		(handle.arena_id == self.pool.arena_id).then(|| self.get_locator(handle.locator)).flatten()
	}

	/// Validates one exact frame generation before exposing immutable bytes.
	pub(super) fn get_locator<'epoch>(
		&'epoch self,
		locator: FrameLocator,
	) -> Option<AtomicEpochPage<'epoch>> {
		let meta = self.pool.metadata.get(locator.index)?;
		let state = meta.state.load(Ordering::SeqCst);
		if state & LIFECYCLE_MASK != Lifecycle::Resident as u64
			|| generation(state) != locator.generation
		{
			return None;
		}
		// SAFETY: entering this epoch precedes the SeqCst resident-generation
		// check. Replacement first publishes `Evicting` with SeqCst and can
		// reuse those `UnsafeCell` bytes only after the global reader count
		// reaches zero. The returned lifetime is tied to this borrowed epoch,
		// so safe code cannot drop the epoch while this immutable reference
		// remains usable.
		let page = unsafe { &*self.pool.frames[locator.index].page.get() };
		Some(AtomicEpochPage {
			page,
		})
	}
}

impl Drop for AtomicReadEpoch<'_> {
	/// Leaves the reclamation epoch exactly once.
	fn drop(&mut self) {
		self.pool.release_read_epoch();
	}
}

/// Page reference whose lifetime cannot exceed its reclamation epoch.
pub struct AtomicEpochPage<'epoch> {
	/// Immutable validated bytes in the stable frame arena.
	page: &'epoch AlignedPage,
}

impl AtomicEpochPage<'_> {
	/// Returns the validated aligned page.
	pub const fn as_page(&self) -> &AlignedPage {
		self.page
	}

	/// Returns the validated page bytes without copying them.
	pub fn as_bytes(&self) -> &[u8; PAGE_SIZE] {
		self.page.as_bytes()
	}
}

/// Borrowed pin guard over immutable arena bytes.
pub struct AtomicPageGuard<'pool> {
	/// Pool that owns both the stable frame and its pin counter.
	pub(super) pool: &'pool AtomicArenaBufferPool,
	/// Exact pinned frame generation.
	pub(super) locator: FrameLocator,
}

impl AtomicPageGuard<'_> {
	/// Returns the pinned immutable aligned page.
	pub fn as_page(&self) -> &AlignedPage {
		let state = self.pool.metadata[self.locator.index].state.load(Ordering::Acquire);
		debug_assert_eq!(lifecycle(state), Lifecycle::Resident);
		debug_assert_eq!(generation(state), self.locator.generation);
		debug_assert!(pins(state) > 0);
		// SAFETY: this guard owns one pin for the exact resident generation.
		// Replacement cannot enter `Evicting` or `Loading` until every pin
		// drops, so no mutable reference can coexist with this immutable
		// reference.
		unsafe { &*self.pool.frames[self.locator.index].page.get() }
	}

	/// Returns the pinned page bytes without copying them.
	pub fn as_bytes(&self) -> &[u8; PAGE_SIZE] {
		self.as_page().as_bytes()
	}
}

impl Drop for AtomicPageGuard<'_> {
	/// Releases the exact generation pin.
	fn drop(&mut self) {
		self.pool.release_pin(self.locator);
	}
}

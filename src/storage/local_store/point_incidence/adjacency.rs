//! Bounded physical adjacency reads and generation-aware membership merges.

use std::collections::BTreeSet;

use super::{AdjacencyChange, IncidencePlan, NodeChanges, NodeSide, NodeTarget, invalid};
use crate::{
	gql_status::FerrosGrynnResult,
	graph::{AtomId, AtomKind, EdgeTopology},
	storage::{
		graph_pages::{
			OVERFLOW_REFS_PER_PAGE,
			decode_adjacency_overflow,
			encode_adjacency_overflow_into,
		},
		local_store::point_occurrence::{SlotTransition, slot_state},
		page::{AlignedPage, PAGE_SIZE, PageLsn, PageNo},
		types::PageKey,
	},
	values::storage::DiskAtomRef,
};

impl IncidencePlan<'_, '_> {
	/// Reads the exact declared side, admitting buffers and cycle tracking
	/// first.
	///
	/// # Errors
	/// Rejects count/chain/ceiling mismatches, admission and page-read
	/// failures.
	pub(super) fn read_side<const N: usize>(
		&mut self,
		inline: &[Option<DiskAtomRef>; N],
		count: u32,
		overflow: Option<PageNo>,
	) -> FerrosGrynnResult<Vec<DiskAtomRef>> {
		let count =
			usize::try_from(count).map_err(|_error| invalid("adjacency count exceeds usize"))?;
		self.budget.retain(
			count
				.checked_mul(size_of::<DiskAtomRef>())
				.ok_or_else(|| invalid("adjacency source bytes overflow usize"))?,
		)?;
		let mut result = Vec::new();
		result
			.try_reserve_exact(count)
			.map_err(|_error| invalid("adjacency source allocation failed"))?;
		for entry in inline.iter().take(count.min(N)) {
			result.push(entry.ok_or_else(|| invalid("adjacency inline entry is absent"))?);
		}
		if inline.iter().skip(count.min(N)).any(Option::is_some)
			|| (count > N) != overflow.is_some()
		{
			return Err(invalid("adjacency inline/count/overflow disagree"));
		}
		let mut visited = BTreeSet::new();
		let mut current = overflow;
		while let Some(number) = current {
			if number.get() >= self.header.adjacency_overflow_pages || visited.contains(&number) {
				return Err(invalid("adjacency source exceeds its ceiling or contains a cycle"));
			}
			self.budget.tree_entry::<PageNo>(visited.len())?;
			visited.insert(number);
			self.budget.scratch(PAGE_SIZE + OVERFLOW_REFS_PER_PAGE * size_of::<DiskAtomRef>())?;
			let page = self.store.read_update_page(
				self.state,
				PageKey::new(self.files.space(), self.files.adjacency, number),
			)?;
			let (next, entries) = decode_adjacency_overflow(&page, number)?;
			if entries.len() > count - result.len() {
				return Err(invalid("adjacency chain exceeds its declared count"));
			}
			result.extend(entries);
			current = next;
		}
		if result.len() != count {
			return Err(invalid("adjacency chain is shorter than its declared count"));
		}
		Ok(result)
	}

	/// Appends final frames after the WAL-visible prefix, retaining old chains.
	///
	/// # Errors
	/// Rejects page-address overflow, admission, allocation and codec failures.
	pub(super) fn append_side(
		&mut self,
		entries: &[DiskAtomRef],
	) -> FerrosGrynnResult<Option<PageNo>> {
		if entries.is_empty() {
			return Ok(None);
		}
		let count = u64::try_from(entries.len().div_ceil(OVERFLOW_REFS_PER_PAGE))
			.map_err(|_error| invalid("adjacency output page count exceeds u64"))?;
		let end = self
			.adjacency_end
			.checked_add(count)
			.filter(|end| *end < u64::MAX)
			.ok_or_else(|| invalid("adjacency append reaches the null page sentinel"))?;
		let first = self.adjacency_end;
		for (offset, chunk) in entries.chunks(OVERFLOW_REFS_PER_PAGE).enumerate() {
			let number = first + offset as u64;
			self.reserve_page()?;
			let mut page = Box::new(AlignedPage::zeroed());
			encode_adjacency_overflow_into(
				&mut page,
				PageNo::new(number),
				PageLsn::ZERO,
				(number + 1 < end).then_some(PageNo::new(number + 1)),
				chunk,
			)?;
			self.pages.push((
				PageKey::new(self.files.space(), self.files.adjacency, PageNo::new(number)),
				page,
			));
		}
		// Old snapshots can still reach old overflow pages; only rebuild
		// reclaims them.
		self.adjacency_end = end;
		Ok(Some(PageNo::new(first)))
	}

	/// Derives only the NODE prerequisites of explicit old/final EDGE slots.
	///
	/// # Errors
	/// Rejects null endpoints, phase identity mismatches and allocation
	/// limits.
	pub(super) fn record_changes(
		&mut self,
		changes: &mut NodeChanges,
		edge: AtomId,
		topology: &EdgeTopology,
		inserted: bool,
	) -> FerrosGrynnResult<()> {
		let mut append = |endpoint: &crate::graph::EdgeEndpoint, side| -> FerrosGrynnResult<()> {
			// Qualified terminals belong to the root endpoint tree, not child
			// NODE adjacency.
			let Some(node) = endpoint.local_id() else {
				return Ok(());
			};
			let kind = node.kind().ok_or_else(|| invalid("plain edge endpoint is null"))?;
			// EDGE incidences belong to the endpoint tree, never NODE
			// adjacency.
			if kind == AtomKind::Edge {
				return Ok(());
			}

			let target_count = changes.len();
			if let std::collections::btree_map::Entry::Vacant(entry) = changes.entry(node.slot()) {
				self.budget.tree_entry::<(u64, NodeTarget)>(target_count)?;
				entry.insert(NodeTarget {
					transition: SlotTransition {
						graph: self.files.graph_id,
						kind: AtomKind::Node,
						slot: node.slot(),
						before: slot_state(
							self.before,
							self.files.graph_id,
							AtomKind::Node,
							node.slot(),
						)?,
						after: slot_state(
							self.after,
							self.files.graph_id,
							AtomKind::Node,
							node.slot(),
						)?,
					},
					explicit: false,
					entries: Vec::new(),
				});
			}
			let target = changes
				.get_mut(&node.slot())
				.ok_or_else(|| invalid("node adjacency target disappeared"))?;
			let phase = if inserted {
				target.transition.after
			} else {
				target.transition.before
			};
			if phase.and_then(|state| state.occurrence).map(|(local, _)| local) != Some(node) {
				return Err(invalid("EDGE endpoint disagrees with its NODE allocator phase"));
			}
			self.budget.retain(2 * size_of::<AdjacencyChange>())?;
			target
				.entries
				.try_reserve_exact(1)
				.map_err(|_error| invalid("node delta allocation failed"))?;
			target.entries.push(AdjacencyChange {
				edge,
				side,
				inserted,
			});
			Ok(())
		};
		match topology {
			EdgeTopology::Directed {
				source,
				target,
			} => {
				for endpoint in source {
					append(endpoint, NodeSide::Outgoing)?;
				}
				for endpoint in target {
					append(endpoint, NodeSide::Incoming)?;
				}
			}
			EdgeTopology::Bidirectional {
				source,
				target,
			} => {
				for endpoint in source.iter().chain(target) {
					append(endpoint, NodeSide::Outgoing)?;
					append(endpoint, NodeSide::Incoming)?;
				}
			}
			EdgeTopology::Undirected {
				endpoints,
			} => {
				for endpoint in endpoints {
					append(endpoint, NodeSide::Undirected)?;
				}
			}
		}
		Ok(())
	}
}

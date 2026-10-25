//! Side layout, checked source chains and bounded unpublished endpoint appends.

use std::collections::BTreeSet;

use super::{IncidencePlan, invalid};
use crate::{
	gql_status::FerrosGrynnResult,
	storage::{
		graph_pages::{
			CROSS_LEVEL_REFS_PER_PAGE,
			EdgeHotSlot,
			EncodedEndpoint,
			EncodedHotSide,
			EndpointSide,
			HotSideOverflow,
			QualifiedDiskAtomRef,
			decode_cross_level_overflow,
			encode_cross_level_overflow,
			plan_hot_side,
		},
		page::{AlignedPage, PAGE_SIZE, PageLsn, PageNo},
		types::PageKey,
	},
	values::storage::DiskAtomRef,
};

impl IncidencePlan<'_, '_> {
	/// Checks an old side or appends a final side using the shared layout
	/// policy.
	///
	/// # Errors
	/// Rejects source layout/count/identity disagreements and admission or I/O
	/// failures.
	pub(super) fn endpoint_side(
		&mut self,
		entries: &[EncodedEndpoint],
		side: EndpointSide,
		cross_level: bool,
		source: Option<&EdgeHotSlot>,
	) -> FerrosGrynnResult<EncodedHotSide> {
		let plan = plan_hot_side(entries, cross_level);
		let mut output = EncodedHotSide {
			inline: plan.inline,
			graph_slot: plan.graph_slot,
			local_overflow: None,
			graph_overflow: None,
		};
		if let Some(source) = source {
			let (count, inline, graph_slot, local_overflow, graph_overflow) = match side {
				EndpointSide::Source => (
					source.source_count,
					source.source_inline,
					source.endpoint_graph_slot(EndpointSide::Source),
					source.source_overflow,
					source.source_subgraph_overflow,
				),
				EndpointSide::Target => (
					source.target_count,
					source.target_inline,
					source.endpoint_graph_slot(EndpointSide::Target),
					source.target_overflow,
					source.target_subgraph_overflow,
				),
			};
			if usize::try_from(count).map_err(|_error| invalid("EDGE side count exceeds usize"))?
				!= entries.len()
				|| inline != output.inline
				|| graph_slot != output.graph_slot
			{
				return Err(invalid("EDGE source side layout disagrees with accepted topology"));
			}
			match plan.overflow {
				HotSideOverflow::None => {
					if local_overflow.is_some() || graph_overflow.is_some() {
						return Err(invalid("inline EDGE source side has an overflow chain"));
					}
				}
				HotSideOverflow::LocalSuffix(_) => {
					if graph_overflow.is_some() {
						return Err(invalid("ordinary EDGE side has qualified overflow"));
					}
					let actual = self.read_side(&inline, count, local_overflow)?;
					if !actual.iter().zip(entries).all(|(actual, expected)| {
						*actual == expected.atom && expected.graph_slot == 0
					}) {
						return Err(invalid(
							"EDGE physical endpoints disagree with accepted topology",
						));
					}
				}
				HotSideOverflow::QualifiedWhole(_) => {
					if local_overflow.is_some() {
						return Err(invalid("qualified EDGE side has ordinary overflow"));
					}
					self.validate_qualified_side(entries, side, graph_overflow)?;
				}
			}
			output.local_overflow = local_overflow;
			output.graph_overflow = graph_overflow;
		} else {
			match plan.overflow {
				HotSideOverflow::None => {}
				HotSideOverflow::LocalSuffix(entries) => {
					output.local_overflow = self.append_local_endpoints(entries)?;
				}
				HotSideOverflow::QualifiedWhole(entries) => {
					output.graph_overflow = self.append_qualified_side(entries, side)?;
				}
			}
		}
		Ok(output)
	}

	/// Uses the existing ordinary codec after admitting the retained suffix
	/// buffer.
	///
	/// # Errors
	/// Rejects byte overflow, resource admission, allocation and page-output
	/// failures.
	fn append_local_endpoints(
		&mut self,
		entries: &[EncodedEndpoint],
	) -> FerrosGrynnResult<Option<PageNo>> {
		self.budget.retain(
			entries
				.len()
				.checked_mul(size_of::<DiskAtomRef>())
				.ok_or_else(|| invalid("EDGE endpoint output bytes overflow"))?,
		)?;
		let mut references = Vec::new();
		references
			.try_reserve_exact(entries.len())
			.map_err(|_error| invalid("EDGE endpoint output allocation failed"))?;
		let excess = references
			.capacity()
			.checked_sub(entries.len())
			.and_then(|count| count.checked_mul(size_of::<DiskAtomRef>()))
			.ok_or_else(|| invalid("EDGE endpoint output capacity overflows"))?;
		self.budget.retain(excess)?;
		references.extend(entries.iter().map(|entry| entry.atom));
		self.append_side(&references)
	}

	/// Streams comparison within the old header ceiling without copying the
	/// whole role.
	///
	/// # Errors
	/// Rejects cycles, wrong scope/side/address, noncanonical lengths,
	/// admission and I/O failures.
	fn validate_qualified_side(
		&mut self,
		expected: &[EncodedEndpoint],
		side: EndpointSide,
		mut current: Option<PageNo>,
	) -> FerrosGrynnResult<()> {
		let mut offset = 0;
		let mut visited = BTreeSet::new();
		while let Some(number) = current {
			if number.get() >= self.header.cross_level_adjacency_overflow_pages
				|| visited.contains(&number)
				|| offset == expected.len()
			{
				return Err(invalid(
					"qualified EDGE chain exceeds its source ceiling/count or contains a cycle",
				));
			}
			self.budget.tree_entry::<PageNo>(visited.len())?;
			visited.insert(number);
			self.budget.scratch(
				PAGE_SIZE + CROSS_LEVEL_REFS_PER_PAGE * size_of::<QualifiedDiskAtomRef>(),
			)?;
			let page = self.store.read_update_page(
				self.state,
				PageKey::new(self.files.space(), self.files.cross_level_adjacency, number),
			)?;
			let (next, entries) = decode_cross_level_overflow(&page, number)?;
			let remaining = expected
				.get(offset..)
				.and_then(|tail| tail.get(..entries.len()))
				.ok_or_else(|| invalid("qualified EDGE chain exceeds its declared count"))?;
			if entries.is_empty()
				|| !entries.iter().zip(remaining).all(|(actual, expected)| {
					actual.side == side
						&& actual.graph_slot == expected.graph_slot
						&& actual.atom == expected.atom
				}) {
				return Err(invalid("qualified EDGE chain disagrees with accepted topology"));
			}
			offset += entries.len();
			current = next;
		}
		if offset != expected.len() {
			return Err(invalid("qualified EDGE chain is shorter than its declared count"));
		}
		Ok(())
	}

	/// Appends complete qualified roles after the WAL-visible tail, retaining
	/// old chains.
	///
	/// # Errors
	/// Rejects sentinel/size overflow, resource admission, allocation, codec
	/// and I/O failures.
	fn append_qualified_side(
		&mut self,
		entries: &[EncodedEndpoint],
		side: EndpointSide,
	) -> FerrosGrynnResult<Option<PageNo>> {
		if entries.is_empty() {
			return Ok(None);
		}
		let first = match self.qualified_end {
			Some(end) => end,
			None => self.store.visible_pages(
				self.state,
				self.files.space(),
				self.files.cross_level_adjacency,
			)?,
		};
		if first < self.header.cross_level_adjacency_overflow_pages {
			return Err(invalid("qualified EDGE source is shorter than its header"));
		}
		let count = u64::try_from(entries.len().div_ceil(CROSS_LEVEL_REFS_PER_PAGE))
			.map_err(|_error| invalid("qualified EDGE output page count exceeds u64"))?;
		let end = first
			.checked_add(count)
			.filter(|end| *end < u64::MAX)
			.ok_or_else(|| invalid("qualified EDGE append reaches the null page sentinel"))?;
		for (offset, chunk) in entries.chunks(CROSS_LEVEL_REFS_PER_PAGE).enumerate() {
			let number = first + offset as u64;
			self.reserve_page()?;
			// No scratch buffer survives another output/WAL reservation in the
			// next iteration.
			self.budget.scratch(2 * PAGE_SIZE + chunk.len() * size_of::<QualifiedDiskAtomRef>())?;
			let mut references = Vec::new();
			references
				.try_reserve_exact(chunk.len())
				.map_err(|_error| invalid("qualified EDGE output allocation failed"))?;
			let scratch = references
				.capacity()
				.checked_mul(size_of::<QualifiedDiskAtomRef>())
				.and_then(|bytes| bytes.checked_add(2 * PAGE_SIZE))
				.ok_or_else(|| invalid("qualified EDGE scratch capacity overflows"))?;
			self.budget.scratch(scratch)?;
			references.extend(chunk.iter().map(|entry| QualifiedDiskAtomRef {
				graph_slot: entry.graph_slot,
				side,
				atom: entry.atom,
			}));
			let page: Box<AlignedPage> = Box::new(encode_cross_level_overflow(
				PageNo::new(number),
				PageLsn::ZERO,
				(number + 1 < end).then_some(PageNo::new(number + 1)),
				&references,
			)?);
			self.pages.push((
				PageKey::new(
					self.files.space(),
					self.files.cross_level_adjacency,
					PageNo::new(number),
				),
				page,
			));
		}
		self.qualified_end = Some(end);
		Ok(Some(PageNo::new(first)))
	}
}

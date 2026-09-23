//! Exact candidate-v1 codec for graph-local checkpoint metadata.

use super::{
	graph_pages::HOT_SLOTS_PER_PAGE,
	page::{AlignedPage, PageFlags, PageLsn, PageNo, PageType, STORAGE_FORMAT_VERSION},
	types::{CheckpointGeneration, Lsn},
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::GraphId,
};

const SUPERBLOCK_PAGE_NO: PageNo = PageNo::new(0);
const FORMAT_VERSION: u32 = STORAGE_FORMAT_VERSION as u32;
const DATA_SPACE_OFFSET: usize = 32;
const FORMAT_VERSION_OFFSET: usize = 40;
const BACK_COMPAT_VERSION_OFFSET: usize = 44;
const CHECKPOINT_GENERATION_OFFSET: usize = 48;
const BACKFILLED_LSN_OFFSET: usize = 56;
const CHECKPOINT_RECORD_LSN_OFFSET: usize = 64;
const NODE_CAPACITY_OFFSET: usize = 72;
const EDGE_CAPACITY_OFFSET: usize = 80;
const NODE_COUNT_OFFSET: usize = 88;
const EDGE_COUNT_OFFSET: usize = 96;
const SUBGRAPH_CAPACITY_OFFSET: usize = 104;
const SUBGRAPH_COUNT_OFFSET: usize = 112;
const NODE_HOT_PAGES_OFFSET: usize = 120;
const EDGE_HOT_PAGES_OFFSET: usize = 128;
const ADJACENCY_PAGES_OFFSET: usize = 136;
const CROSS_LEVEL_PAGES_OFFSET: usize = 144;
const OVERLAYS_PAGES_OFFSET: usize = 152;
const SUBGRAPHS_PAGES_OFFSET: usize = 160;
const LABELS_PAGES_OFFSET: usize = 168;
const RESERVED_OFFSET: usize = 176;

/// Complete logical metadata for one graph data space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct GraphSuperblock {
	/// WAL LSN of the logical metadata image.
	pub(super) page_lsn: PageLsn,
	/// Non-zero graph data-space identity.
	pub(super) graph_id: GraphId,
	/// Checkpoint generation authorized by database metadata.
	pub(super) checkpoint_generation: CheckpointGeneration,
	/// Highest WAL boundary copied into home files.
	pub(super) backfilled_lsn: Lsn,
	/// Matching durable `CheckpointEnd` record.
	pub(super) checkpoint_record_lsn: Lsn,
	/// Allocated node occurrence slots.
	pub(super) node_capacity: u64,
	/// Allocated edge occurrence slots.
	pub(super) edge_capacity: u64,
	/// Live node occurrences.
	pub(super) node_count: u64,
	/// Live edge occurrences.
	pub(super) edge_count: u64,
	/// Allocated nested-graph directory slots.
	pub(super) subgraph_capacity: u64,
	/// Live nested-graph directory entries.
	pub(super) subgraph_count: u64,
	/// Snapshot-visible `nodes_hot.fgb` page ceiling.
	pub(super) node_hot_pages: u64,
	/// Snapshot-visible `edges_hot.fgb` page ceiling.
	pub(super) edge_hot_pages: u64,
	/// Snapshot-visible local adjacency overflow ceiling.
	pub(super) adjacency_overflow_pages: u64,
	/// Snapshot-visible qualified adjacency overflow ceiling.
	pub(super) cross_level_adjacency_overflow_pages: u64,
	/// Snapshot-visible graph-overlay page ceiling.
	pub(super) overlays_pages: u64,
	/// Snapshot-visible nested-graph directory ceiling.
	pub(super) subgraphs_pages: u64,
	/// Snapshot-visible graph-label dictionary ceiling.
	pub(super) labels_pages: u64,
}

impl GraphSuperblock {
	/// Encodes one sealed metadata page at physical page zero.
	pub(super) fn encode(self) -> FerrosGrynnResult<AlignedPage> {
		self.validate_semantics()?;
		let mut page = AlignedPage::new(
			PageType::GraphSuperblock,
			PageFlags::empty(),
			self.page_lsn,
			SUPERBLOCK_PAGE_NO,
		);
		let bytes = page.as_bytes_mut();
		write_u64(bytes, DATA_SPACE_OFFSET, self.graph_id.get());
		write_u32(bytes, FORMAT_VERSION_OFFSET, FORMAT_VERSION);
		write_u32(bytes, BACK_COMPAT_VERSION_OFFSET, FORMAT_VERSION);
		write_u64(bytes, CHECKPOINT_GENERATION_OFFSET, self.checkpoint_generation.get());
		write_u64(bytes, BACKFILLED_LSN_OFFSET, self.backfilled_lsn.get());
		write_u64(bytes, CHECKPOINT_RECORD_LSN_OFFSET, self.checkpoint_record_lsn.get());
		write_u64(bytes, NODE_CAPACITY_OFFSET, self.node_capacity);
		write_u64(bytes, EDGE_CAPACITY_OFFSET, self.edge_capacity);
		write_u64(bytes, NODE_COUNT_OFFSET, self.node_count);
		write_u64(bytes, EDGE_COUNT_OFFSET, self.edge_count);
		write_u64(bytes, SUBGRAPH_CAPACITY_OFFSET, self.subgraph_capacity);
		write_u64(bytes, SUBGRAPH_COUNT_OFFSET, self.subgraph_count);
		write_u64(bytes, NODE_HOT_PAGES_OFFSET, self.node_hot_pages);
		write_u64(bytes, EDGE_HOT_PAGES_OFFSET, self.edge_hot_pages);
		write_u64(bytes, ADJACENCY_PAGES_OFFSET, self.adjacency_overflow_pages);
		write_u64(bytes, CROSS_LEVEL_PAGES_OFFSET, self.cross_level_adjacency_overflow_pages);
		write_u64(bytes, OVERLAYS_PAGES_OFFSET, self.overlays_pages);
		write_u64(bytes, SUBGRAPHS_PAGES_OFFSET, self.subgraphs_pages);
		write_u64(bytes, LABELS_PAGES_OFFSET, self.labels_pages);
		page.seal()?;
		Ok(page)
	}

	/// Decodes one exact graph-superblock page and validates all relations.
	pub(super) fn decode(page: &AlignedPage) -> FerrosGrynnResult<Self> {
		let header = page.validate(SUPERBLOCK_PAGE_NO)?;
		if header.page_type() != PageType::GraphSuperblock || !header.flags().is_empty() {
			return Err(invalid("invalid GraphSuperblock page type or flags"));
		}
		let bytes = page.as_bytes();
		let format = read_u32(bytes, FORMAT_VERSION_OFFSET);
		let compatible = read_u32(bytes, BACK_COMPAT_VERSION_OFFSET);
		if format != FORMAT_VERSION || compatible != FORMAT_VERSION {
			return Err(FerrosGrynnError::unsupported_storage_format(
				u64::from(format),
				u64::from(FORMAT_VERSION),
			));
		}
		if bytes[RESERVED_OFFSET..].iter().any(|byte| *byte != 0) {
			return Err(invalid("GraphSuperblock reserved bytes are non-zero"));
		}
		let graph_id = GraphId::new(read_u64(bytes, DATA_SPACE_OFFSET))
			.ok_or_else(|| invalid("GraphSuperblock uses database data-space identity zero"))?;
		let value = Self {
			page_lsn: header.page_lsn(),
			graph_id,
			checkpoint_generation: CheckpointGeneration::new(read_u64(
				bytes,
				CHECKPOINT_GENERATION_OFFSET,
			)),
			backfilled_lsn: Lsn::new(read_u64(bytes, BACKFILLED_LSN_OFFSET)),
			checkpoint_record_lsn: Lsn::new(read_u64(bytes, CHECKPOINT_RECORD_LSN_OFFSET)),
			node_capacity: read_u64(bytes, NODE_CAPACITY_OFFSET),
			edge_capacity: read_u64(bytes, EDGE_CAPACITY_OFFSET),
			node_count: read_u64(bytes, NODE_COUNT_OFFSET),
			edge_count: read_u64(bytes, EDGE_COUNT_OFFSET),
			subgraph_capacity: read_u64(bytes, SUBGRAPH_CAPACITY_OFFSET),
			subgraph_count: read_u64(bytes, SUBGRAPH_COUNT_OFFSET),
			node_hot_pages: read_u64(bytes, NODE_HOT_PAGES_OFFSET),
			edge_hot_pages: read_u64(bytes, EDGE_HOT_PAGES_OFFSET),
			adjacency_overflow_pages: read_u64(bytes, ADJACENCY_PAGES_OFFSET),
			cross_level_adjacency_overflow_pages: read_u64(bytes, CROSS_LEVEL_PAGES_OFFSET),
			overlays_pages: read_u64(bytes, OVERLAYS_PAGES_OFFSET),
			subgraphs_pages: read_u64(bytes, SUBGRAPHS_PAGES_OFFSET),
			labels_pages: read_u64(bytes, LABELS_PAGES_OFFSET),
		};
		value.validate_semantics()?;
		Ok(value)
	}

	/// Returns this logical image with a completed checkpoint tuple.
	pub(super) fn at_checkpoint(
		mut self,
		generation: CheckpointGeneration,
		backfilled_lsn: Lsn,
		checkpoint_record_lsn: Lsn,
	) -> FerrosGrynnResult<Self> {
		self.checkpoint_generation = generation;
		self.backfilled_lsn = backfilled_lsn;
		self.checkpoint_record_lsn = checkpoint_record_lsn;
		self.validate_semantics()?;
		if self.page_lsn.get() > backfilled_lsn.get() {
			return Err(invalid("checkpointed GraphSuperblock LSN exceeds backfill boundary"));
		}
		Ok(self)
	}

	/// Compares transaction-visible metadata independently of its WAL frame
	/// LSN.
	pub(super) fn same_logical_metadata(self, other: Self) -> bool {
		Self {
			page_lsn: PageLsn::ZERO,
			..self
		} == Self {
			page_lsn: PageLsn::ZERO,
			..other
		}
	}

	fn validate_semantics(self) -> FerrosGrynnResult<()> {
		if self.node_count > self.node_capacity
			|| self.edge_count > self.edge_capacity
			|| self.subgraph_count > self.subgraph_capacity
			|| self.subgraph_capacity > u64::from(u32::MAX)
		{
			return Err(invalid("GraphSuperblock live count exceeds its capacity"));
		}
		let per_page = u64::try_from(HOT_SLOTS_PER_PAGE)
			.map_err(|_error| invalid("hot-page slot count does not fit u64"))?;
		if self.node_hot_pages != self.node_capacity.div_ceil(per_page)
			|| self.edge_hot_pages != self.edge_capacity.div_ceil(per_page)
		{
			return Err(invalid("GraphSuperblock hot-page ceiling disagrees with capacity"));
		}
		if self.subgraphs_pages == 0 || self.labels_pages == 0 {
			return Err(invalid("graph subgraph and label roots must contain page zero"));
		}
		if self.checkpoint_generation == CheckpointGeneration::ZERO {
			if self.backfilled_lsn != Lsn::ZERO || self.checkpoint_record_lsn != Lsn::ZERO {
				return Err(invalid("generation zero requires zero checkpoint clocks"));
			}
		} else if self.backfilled_lsn == Lsn::ZERO
			|| self.checkpoint_record_lsn == Lsn::ZERO
			|| self.backfilled_lsn > self.checkpoint_record_lsn
		{
			return Err(invalid("GraphSuperblock checkpoint tuple is inconsistent"));
		}
		Ok(())
	}
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
	bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
	bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
	let mut value = [0; 4];
	value.copy_from_slice(&bytes[offset..offset + 4]);
	u32::from_le_bytes(value)
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
	let mut value = [0; 8];
	value.copy_from_slice(&bytes[offset..offset + 8]);
	u64::from_le_bytes(value)
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn fixture() -> GraphSuperblock {
		GraphSuperblock {
			page_lsn: PageLsn::new(13),
			graph_id: GraphId::new(7).unwrap(),
			checkpoint_generation: CheckpointGeneration::new(3),
			backfilled_lsn: Lsn::new(17),
			checkpoint_record_lsn: Lsn::new(19),
			node_capacity: 103,
			edge_capacity: 102,
			node_count: 4,
			edge_count: 2,
			subgraph_capacity: 1,
			subgraph_count: 1,
			node_hot_pages: 2,
			edge_hot_pages: 1,
			adjacency_overflow_pages: 2,
			cross_level_adjacency_overflow_pages: 0,
			overlays_pages: 3,
			subgraphs_pages: 1,
			labels_pages: 1,
		}
	}

	#[test]
	fn exact_layout_round_trips() -> FerrosGrynnResult<()> {
		let expected = fixture();
		let page = expected.encode()?;
		assert_eq!(read_u64(page.as_bytes(), DATA_SPACE_OFFSET), 7);
		assert_eq!(read_u64(page.as_bytes(), NODE_CAPACITY_OFFSET), 103);
		assert_eq!(read_u64(page.as_bytes(), LABELS_PAGES_OFFSET), 1);
		assert_eq!(GraphSuperblock::decode(&page)?, expected);
		Ok(())
	}

	#[test]
	fn rejects_zero_space_reserved_bytes_and_invalid_ceilings() -> FerrosGrynnResult<()> {
		let mut page = fixture().encode()?;
		write_u64(page.as_bytes_mut(), DATA_SPACE_OFFSET, 0);
		page.seal()?;
		assert!(GraphSuperblock::decode(&page).is_err());
		let mut page = fixture().encode()?;
		page.as_bytes_mut()[RESERVED_OFFSET] = 1;
		page.seal()?;
		assert!(GraphSuperblock::decode(&page).is_err());
		let mut invalid = fixture();
		invalid.node_hot_pages = 1;
		assert!(invalid.encode().is_err());
		Ok(())
	}
}

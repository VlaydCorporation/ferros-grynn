//! Shared typed identities and clocks for local disk storage.

use super::page::PageNo;
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// First assignable WAL record position.
const FIRST_LSN: u64 = 1;

/// Largest instance identity representable by [`DataFileId`].
const MAX_FILE_INSTANCE_ID: u64 = u64::MAX >> u8::BITS;

/// A non-zero position in the database WAL record stream.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub(super) struct Lsn(u64);

impl Lsn {
	/// First assignable WAL record position.
	pub(super) const FIRST: Self = Self(FIRST_LSN);
	/// Sentinel used when no WAL record exists yet.
	pub(super) const ZERO: Self = Self(0);

	/// Creates an LSN from a persisted integer.
	pub(super) const fn new(value: u64) -> Self {
		Self(value)
	}

	/// Returns the persisted integer value.
	pub(super) const fn get(self) -> u64 {
		self.0
	}

	/// Returns the next non-zero WAL position.
	///
	/// # Errors
	///
	/// Returns a storage-format error when the clock is exhausted.
	pub(super) fn checked_next(self) -> FerrosGrynnResult<Self> {
		self.0
			.checked_add(1)
			.map(Self)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("the WAL LSN clock is exhausted"))
	}
}

/// Database-wide logical commit clock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub(super) struct CommitTs(u64);

impl CommitTs {
	/// Clock value before the first committed transaction.
	pub(super) const ZERO: Self = Self(0);

	/// Creates a commit timestamp from a persisted integer.
	pub(super) const fn new(value: u64) -> Self {
		Self(value)
	}

	/// Returns the persisted integer value.
	pub(super) const fn get(self) -> u64 {
		self.0
	}
}

/// Database-wide transaction identity stored in WAL headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub(super) struct TxId(u64);

impl TxId {
	/// Reserved identity for database-level maintenance records.
	pub(super) const SYSTEM: Self = Self(0);

	/// Creates a transaction identity from a persisted integer.
	pub(super) const fn new(value: u64) -> Self {
		Self(value)
	}

	/// Returns the persisted integer value.
	pub(super) const fn get(self) -> u64 {
		self.0
	}
}

/// Stable database graph identity used in a physical page key.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub(super) struct GraphId(u64);

impl GraphId {
	/// System catalog and database metadata address space.
	pub(super) const SYSTEM: Self = Self(0);

	/// Creates a graph identity from its stable integer.
	pub(super) const fn new(value: u64) -> Self {
		Self(value)
	}

	/// Returns the persisted integer value.
	pub(super) const fn get(self) -> u64 {
		self.0
	}
}

/// Monotonic checkpoint generation committed by the database superblock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub(super) struct CheckpointGeneration(u64);

impl CheckpointGeneration {
	/// Generation before the first completed checkpoint.
	pub(super) const ZERO: Self = Self(0);

	/// Creates a checkpoint generation from a persisted integer.
	pub(super) const fn new(value: u64) -> Self {
		Self(value)
	}

	/// Returns the persisted integer value.
	pub(super) const fn get(self) -> u64 {
		self.0
	}

	/// Returns the next checkpoint generation.
	///
	/// # Errors
	///
	/// Returns a storage-format error when the generation clock is exhausted.
	pub(super) fn checked_next(self) -> FerrosGrynnResult<Self> {
		self.0.checked_add(1).map(Self).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("the checkpoint generation clock is exhausted")
		})
	}
}

/// Closed format-v1 physical file role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub(super) enum DataFileKind {
	/// Database system catalog.
	SystemCatalog = 0x01,
	/// Checkpoint-owned graph superblock.
	GraphSuperblock = 0x02,
	/// Fixed node hot pages.
	NodesHot = 0x03,
	/// Fixed edge hot pages.
	EdgesHot = 0x04,
	/// Same-level adjacency overflow pages.
	AdjacencyOverflow = 0x05,
	/// Cross-level adjacency overflow pages.
	CrossLevelAdjacencyOverflow = 0x06,
	/// Property heap pages.
	Properties = 0x07,
	/// Subgraph directory pages.
	Subgraphs = 0x08,
	/// Label dictionary pages.
	Labels = 0x09,
	/// Freelist pages.
	Freelist = 0x0a,
	/// Statistics pages.
	Statistics = 0x0b,
	/// One index instance.
	Index = 0x0c,
	/// Derived tensor-cache pages.
	TensorCache = 0x0d,
}

impl DataFileKind {
	/// Returns the assigned format-v1 discriminant.
	pub(super) const fn get(self) -> u8 {
		self as u8
	}

	/// Decodes only format-v1 assigned discriminants.
	pub(super) const fn from_raw(value: u8) -> Option<Self> {
		if value == Self::SystemCatalog.get() {
			Some(Self::SystemCatalog)
		} else if value == Self::GraphSuperblock.get() {
			Some(Self::GraphSuperblock)
		} else if value == Self::NodesHot.get() {
			Some(Self::NodesHot)
		} else if value == Self::EdgesHot.get() {
			Some(Self::EdgesHot)
		} else if value == Self::AdjacencyOverflow.get() {
			Some(Self::AdjacencyOverflow)
		} else if value == Self::CrossLevelAdjacencyOverflow.get() {
			Some(Self::CrossLevelAdjacencyOverflow)
		} else if value == Self::Properties.get() {
			Some(Self::Properties)
		} else if value == Self::Subgraphs.get() {
			Some(Self::Subgraphs)
		} else if value == Self::Labels.get() {
			Some(Self::Labels)
		} else if value == Self::Freelist.get() {
			Some(Self::Freelist)
		} else if value == Self::Statistics.get() {
			Some(Self::Statistics)
		} else if value == Self::Index.get() {
			Some(Self::Index)
		} else if value == Self::TensorCache.get() {
			Some(Self::TensorCache)
		} else {
			None
		}
	}
}

/// Stable identity of one physical data file.
///
/// The upper 56 bits hold a never-reused instance identity and the low byte
/// holds the closed [`DataFileKind`] discriminant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub(super) struct DataFileId(u64);

impl DataFileId {
	/// First unassignable value used as the exhausted next-id clock.
	pub(super) const EXHAUSTED_INSTANCE_ID: u64 = MAX_FILE_INSTANCE_ID + 1;
	/// Largest format-v1 file instance identity.
	pub(super) const MAX_INSTANCE_ID: u64 = MAX_FILE_INSTANCE_ID;

	/// Creates the canonical zero-instance identity for one singleton family.
	pub(super) const fn for_kind(kind: DataFileKind) -> Self {
		Self(kind.get() as u64)
	}

	/// Creates an identity when `instance_id` fits the format-v1 56-bit field.
	pub(super) const fn new(instance_id: u64, kind: DataFileKind) -> Option<Self> {
		if instance_id > MAX_FILE_INSTANCE_ID {
			None
		} else {
			Some(Self((instance_id << u8::BITS) | kind.get() as u64))
		}
	}

	/// Decodes an identity only when its low byte is an assigned file kind.
	pub(super) const fn from_raw(value: u64) -> Option<Self> {
		let kind = value.to_le_bytes()[0];
		if DataFileKind::from_raw(kind).is_some() {
			Some(Self(value))
		} else {
			None
		}
	}

	/// Returns the complete persisted integer value.
	pub(super) const fn get(self) -> u64 {
		self.0
	}

	/// Returns the 56-bit never-reused instance identity.
	pub(super) const fn instance_id(self) -> u64 {
		self.0 >> u8::BITS
	}

	/// Returns the validated physical file role.
	pub(super) const fn kind(self) -> Option<DataFileKind> {
		DataFileKind::from_raw(self.0.to_le_bytes()[0])
	}
}

/// Complete physical identity of a page in the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct PageKey {
	/// Graph or system address space.
	graph_id: GraphId,
	/// Stable physical data-file identity.
	file_id: DataFileId,
	/// Page number within the physical file.
	page_no: PageNo,
}

impl PageKey {
	/// Creates a physical page key.
	pub(super) const fn new(graph_id: GraphId, file_id: DataFileId, page_no: PageNo) -> Self {
		Self {
			graph_id,
			file_id,
			page_no,
		}
	}

	/// Returns the graph identity.
	pub(super) const fn graph_id(self) -> GraphId {
		self.graph_id
	}

	/// Returns the stable physical file identity.
	pub(super) const fn file_id(self) -> DataFileId {
		self.file_id
	}

	/// Returns the physical page number.
	pub(super) const fn page_no(self) -> PageNo {
		self.page_no
	}
}

/// Durable visibility boundary captured when a transaction begins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StorageSnapshot {
	/// Logical database state visible to higher MVCC layers.
	commit_ts: CommitTs,
	/// Highest durable commit marker visible to this snapshot.
	visible_lsn: Lsn,
}

impl StorageSnapshot {
	/// Creates one immutable storage snapshot.
	pub(super) const fn new(commit_ts: CommitTs, visible_lsn: Lsn) -> Self {
		Self {
			commit_ts,
			visible_lsn,
		}
	}

	/// Returns the captured logical commit timestamp.
	pub(super) const fn commit_ts(self) -> CommitTs {
		self.commit_ts
	}

	/// Returns the captured durable WAL boundary.
	pub(super) const fn visible_lsn(self) -> Lsn {
		self.visible_lsn
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Data-file identities preserve both the family and 56-bit instance.
	#[test]
	fn data_file_id_layout_is_stable_and_validated() {
		let id = DataFileId::new(0x00ab_cdef_0123_4567, DataFileKind::Index);
		assert_eq!(id.map(DataFileId::get), Some(0xabcd_ef01_2345_670c));
		assert_eq!(id.map(DataFileId::instance_id), Some(0x00ab_cdef_0123_4567));
		assert_eq!(id.and_then(DataFileId::kind), Some(DataFileKind::Index));
		assert!(DataFileId::new(MAX_FILE_INSTANCE_ID + 1, DataFileKind::Index).is_none());
		assert!(DataFileId::from_raw(0).is_none());
		assert!(DataFileId::from_raw(0x0e).is_none());
	}

	/// Monotonic persisted clocks fail closed instead of wrapping to reserved
	/// identities.
	#[test]
	fn persisted_clocks_reject_exhaustion() {
		assert!(Lsn::new(u64::MAX).checked_next().is_err());
		assert!(CheckpointGeneration::new(u64::MAX).checked_next().is_err());
	}
}

use std::fmt::{self, Display, Formatter};

use super::{DISK_ATOM_ALLOWED_MASK, DISK_ATOM_KIND_BIT, DISK_ATOM_SLOT_MASK};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::AtomKind,
};

/// Stable tags of canonical storage format version 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TypeTag {
	None = 0x00,
	Null = 0x01,
	False = 0x02,
	True = 0x03,
	Int = 0x04,
	Float = 0x05,
	Decimal = 0x06,
	String = 0x07,
	Bytes = 0x08,
	DateTime = 0x09,
	Duration = 0x0A,
	Uuid = 0x0B,
	Ulid = 0x0C,
	RecordId = 0x10,
	Array = 0x11,
	Set = 0x12,
	Object = 0x13,
	Range = 0x15,
	Point = 0x21,
	LineString = 0x22,
	Polygon = 0x23,
	MultiPoint = 0x24,
	MultiLineString = 0x25,
	MultiPolygon = 0x26,
	GeometryCollection = 0x27,
	VectorF64 = 0x31,
	VectorF32 = 0x32,
	VectorI64 = 0x33,
	VectorI32 = 0x34,
	VectorI16 = 0x35,
	AtomRef = 0x36,
	GraphRef = 0x37,
	QualifiedAtomRef = 0x38,
}

impl TypeTag {
	pub const fn byte(self) -> u8 {
		self as u8
	}
}

/// Generation-free atom address stored on disk.
#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[repr(transparent)]
pub struct DiskAtomRef(u64);

impl DiskAtomRef {
	pub fn from_parts(kind: AtomKind, slot: u64) -> FerrosGrynnResult<Self> {
		if slot > DISK_ATOM_SLOT_MASK {
			return Err(FerrosGrynnError::graph_capacity_exceeded(kind.to_string()));
		}
		let kind = match kind {
			AtomKind::Node => 0,
			AtomKind::Edge => DISK_ATOM_KIND_BIT,
		};
		Ok(Self(kind | slot))
	}

	pub fn from_raw(raw: u64) -> FerrosGrynnResult<Self> {
		if raw & !DISK_ATOM_ALLOWED_MASK != 0 {
			return Err(FerrosGrynnError::invalid_storage_value_encoding(
				0,
				"DiskAtomRef uses reserved high bits",
			));
		}
		Ok(Self(raw))
	}

	pub const fn raw(self) -> u64 {
		self.0
	}

	pub const fn kind(self) -> AtomKind {
		if self.0 & DISK_ATOM_KIND_BIT == 0 {
			AtomKind::Node
		} else {
			AtomKind::Edge
		}
	}

	pub const fn slot(self) -> u64 {
		self.0 & DISK_ATOM_SLOT_MASK
	}
}

impl Display for DiskAtomRef {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		write!(f, "{}:{}", self.kind(), self.slot())
	}
}

#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub struct DiskGraphRef {
	pub(super) slot: u32,
	pub(super) generation: u32,
}

impl DiskGraphRef {
	pub const fn new(slot: u32, generation: u32) -> Self {
		Self {
			slot,
			generation,
		}
	}
}

impl Display for DiskGraphRef {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		write!(f, "{}:{}", self.slot, self.generation)
	}
}

#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub struct QualifiedAtomRef {
	pub(super) graph_slot: u32,
	pub(super) local_ref: DiskAtomRef,
}

impl QualifiedAtomRef {
	pub const fn new(graph_slot: u32, local_ref: DiskAtomRef) -> Self {
		Self {
			graph_slot,
			local_ref,
		}
	}
}

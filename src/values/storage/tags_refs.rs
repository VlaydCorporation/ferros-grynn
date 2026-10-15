use std::fmt::{self, Display, Formatter};

use super::{DISK_ATOM_ALLOWED_MASK, DISK_ATOM_KIND_BIT, DISK_ATOM_SLOT_MASK};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{AtomKind, GraphId},
};

/// Stable tags of canonical storage format version 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TypeTag {
	/// Canonical storage tag identifying an absent value.
	None = 0x00,
	/// Canonical storage tag identifying an explicit null value.
	Null = 0x01,
	/// Canonical storage tag identifying the Boolean false value.
	False = 0x02,
	/// Canonical storage tag identifying the Boolean true value.
	True = 0x03,
	/// Canonical storage tag identifying a signed integer.
	Int = 0x04,
	/// Canonical storage tag identifying a floating-point number.
	Float = 0x05,
	/// Canonical storage tag identifying a decimal number.
	Decimal = 0x06,
	/// Canonical storage tag identifying text.
	String = 0x07,
	/// Canonical storage tag identifying binary bytes.
	Bytes = 0x08,
	/// Canonical storage tag identifying a time-zone-aware timestamp.
	DateTime = 0x09,
	/// Canonical storage tag identifying a duration.
	Duration = 0x0A,
	/// Canonical storage tag identifying a UUID.
	Uuid = 0x0B,
	/// Canonical storage tag identifying a ULID.
	Ulid = 0x0C,
	/// Canonical storage tag identifying a class-qualified record identity.
	RecordId = 0x10,
	/// Canonical storage tag identifying an ordered sequence of values.
	Array = 0x11,
	/// Canonical storage tag identifying an ordered set of distinct values.
	Set = 0x12,
	/// Canonical storage tag identifying a text-keyed value map.
	Object = 0x13,
	/// Canonical storage tag identifying a bounded or unbounded value interval.
	Range = 0x15,
	/// Canonical storage tag identifying a geometry point.
	Point = 0x21,
	/// Canonical storage tag identifying a connected coordinate sequence.
	LineString = 0x22,
	/// Canonical storage tag identifying a polygon geometry.
	Polygon = 0x23,
	/// Canonical storage tag identifying a collection of points.
	MultiPoint = 0x24,
	/// Canonical storage tag identifying a collection of line strings.
	MultiLineString = 0x25,
	/// Canonical storage tag identifying a collection of polygons.
	MultiPolygon = 0x26,
	/// Canonical storage tag identifying a collection of geometry values.
	GeometryCollection = 0x27,
	/// Canonical storage tag identifying 64-bit floating-point vector elements.
	VectorF64 = 0x31,
	/// Canonical storage tag identifying 32-bit floating-point vector elements.
	VectorF32 = 0x32,
	/// Canonical storage tag identifying 64-bit signed integer vector elements.
	VectorI64 = 0x33,
	/// Canonical storage tag identifying 32-bit signed integer vector elements.
	VectorI32 = 0x34,
	/// Canonical storage tag identifying 16-bit signed integer vector elements.
	VectorI16 = 0x35,
	/// Canonical storage tag identifying an atom reference.
	AtomRef = 0x36,
	/// Canonical storage tag identifying a stable graph reference.
	GraphRef = 0x37,
	/// Canonical storage tag identifying a graph-qualified atom reference.
	QualifiedAtomRef = 0x38,
	/// Canonical storage tag identifying 16-bit floating-point vector elements.
	VectorF16 = 0x39,
	/// Canonical storage tag identifying 8-bit signed integer vector elements.
	VectorI8 = 0x3A,
	/// Canonical storage tag identifying 8-bit unsigned integer vector
	/// elements.
	VectorU8 = 0x3B,
}

impl TypeTag {
	/// Returns byte for `TypeTag`.
	pub const fn byte(self) -> u8 {
		self as u8
	}
}

/// Generation-free atom address stored on disk.
#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[repr(transparent)]
pub struct DiskAtomRef(
	/// Packed node-or-edge kind and generation-free slot persisted in the value
	/// format.
	u64,
);

impl DiskAtomRef {
	/// Constructs from parts for `DiskAtomRef`.
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

	/// Constructs from raw for `DiskAtomRef`.
	pub fn from_raw(raw: u64) -> FerrosGrynnResult<Self> {
		if raw & !DISK_ATOM_ALLOWED_MASK != 0 {
			return Err(FerrosGrynnError::invalid_storage_value_encoding(
				0,
				"DiskAtomRef uses reserved high bits",
			));
		}
		Ok(Self(raw))
	}

	/// Returns the wrapped raw representation.
	pub const fn raw(self) -> u64 {
		self.0
	}

	/// Returns kind for `DiskAtomRef`.
	pub const fn kind(self) -> AtomKind {
		if self.0 & DISK_ATOM_KIND_BIT == 0 {
			AtomKind::Node
		} else {
			AtomKind::Edge
		}
	}

	/// Returns slot for `DiskAtomRef`.
	pub const fn slot(self) -> u64 {
		self.0 & DISK_ATOM_SLOT_MASK
	}
}

impl Display for DiskAtomRef {
	/// Formats `DiskAtomRef` using the requested formatter.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		write!(f, "{}:{}", self.kind(), self.slot())
	}
}

/// Compact on-disk identifier resolved to an owned graph reference by
/// conversion context.
#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[repr(transparent)]
pub struct DiskGraphRef(
	/// Numeric on-disk graph reference resolved by the conversion context.
	GraphId,
);

impl DiskGraphRef {
	/// Constructs `DiskGraphRef` from the supplied initial state.
	pub const fn new(graph_id: GraphId) -> Self {
		Self(graph_id)
	}

	/// Constructs from raw for `DiskGraphRef`.
	pub fn from_raw(raw: u64) -> FerrosGrynnResult<Self> {
		GraphId::new(raw).map(Self).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_value_encoding(
				0,
				"graph reference contains reserved graph identity zero",
			)
		})
	}

	/// Returns the wrapped graph id representation.
	pub const fn graph_id(self) -> GraphId {
		self.0
	}

	/// Returns raw for `DiskGraphRef`.
	pub const fn raw(self) -> u64 {
		self.0.get()
	}
}

impl Display for DiskGraphRef {
	/// Formats `DiskGraphRef` using the requested formatter.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		Display::fmt(&self.0, f)
	}
}

/// On-disk graph slot paired with an atom reference local to that graph.
#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub struct QualifiedAtomRef {
	/// On-disk graph slot qualifying the local atom reference.
	pub(super) graph_slot: u32,
	/// Atom reference interpreted within the qualified graph slot.
	pub(super) local_ref: DiskAtomRef,
}

impl QualifiedAtomRef {
	/// Constructs `QualifiedAtomRef` from the supplied initial state.
	pub const fn new(graph_slot: u32, local_ref: DiskAtomRef) -> Self {
		Self {
			graph_slot,
			local_ref,
		}
	}
}

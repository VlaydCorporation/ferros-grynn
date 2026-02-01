use crate::common::memory::Measurable;
use crate::values::error::Result;
use crate::values::mapper::ValueMapper;
use crate::values::virt::{CompositeDatabaseValue, VirtualValueExt};
use crate::values::writer::{AnyValueWriter, EntityMode};
use crate::values::{AnyValueExt, ElementId, Id, ValueRepresentation};
use magic_utils::{Display, IntoVariants};
use std::fmt::{Display, Formatter};
use std::hash::Hash;
use crate::{impl_id_hash, impl_simple_hashable};
use crate::values::memohash::{hash_u64, HashValue, Hashable, MemoHashed, MemoizedHash, HASH_CONSTANT};

//#region ----------------- MACROS -----------------
macro_rules! impl_any_ext {
    ($($ident:ident),+) => {
		$(
			impl AnyValueExt for $ident {
				fn write_to<T: AnyValueWriter>(&self, writer: &mut T) -> Result<()> {
					match writer.entity_mode() {
						EntityMode::Reference => writer.write_node_reference(self.id()),
						EntityMode::Full => writer.write_node(
							self.element_id(),
							self.id(),
							self.labels(),
							self.properties(),
							self.is_deleted(),
						),
					}
				}

				fn map<T>(&self, mapper: &impl ValueMapper<T>) {
					mapper.map_node(self);
				}

				fn representation(&self) -> ValueRepresentation {
					todo!()
				}

				fn get_type_name(&self) -> &str {
					"Node"
				}
			}
		)*
	};
}
//#endregion ----------------- MACROS -----------------

//#region ----------------- TRAITS -----------------
pub trait VirtualNodeValue: VirtualValueExt {
	fn id(&self) -> Id;
}

pub trait VirtualNodeReferenceExt: VirtualNodeValue {
	fn element_id(&self) -> ElementId;
}

pub trait NodeValueExt: VirtualNodeReferenceExt {
	fn labels(&self) -> TextArray;
	fn properties(&self) -> MapValue;
}

//#endregion ----------------- TRAITS -----------------
#[derive(PartialEq, IntoVariants, Display, Measurable)]
pub enum VirtualNodeReference {
	#[magic(to_string = "{0}")]
	Direct(DirectNodeValue),
	#[magic(to_string = "{0}")]
	CompositeDirect(CompositeGraphDirectNodeValue),
	#[magic(to_string = "{0}")]
	FullRef(FullNodeReference),
	#[magic(to_string = "{0}")]
	CompositeFull(CompositeFullNodeReference),
	#[magic(to_string = "{0}")]
	Wrapping(NodeEntityWrappingNodeValue),
}

impl Hashable for VirtualNodeReference {
	fn hash(&self) -> HashValue {
		match self {
			VirtualNodeReference::Direct(d) => d.hash(),
			VirtualNodeReference::CompositeDirect(cd) => cd.hash(),
			VirtualNodeReference::FullRef(fr) => fr.hash(),
			VirtualNodeReference::CompositeFull(cf) => cf.hash(),
			VirtualNodeReference::Wrapping(w) => w.hash(),
		}
	}
}

impl AnyValueExt for VirtualNodeReference {
	fn write_to<T: AnyValueWriter>(&self, writer: &mut T) -> Result<()> {
		match self {
			VirtualNodeReference::Direct(d) => d.write_to(writer),
			VirtualNodeReference::CompositeDirect(cd) => cd.write_to(writer),
			VirtualNodeReference::FullRef(fr) => fr.write_to(writer),
			VirtualNodeReference::CompositeFull(cf) => cf.write_to(writer),
			VirtualNodeReference::Wrapping(w) => w.write_to(writer),
		}
	}

	fn map<T>(&self, mapper: &impl ValueMapper<T>) {
		mapper.map_node(self);
	}

	fn representation(&self) -> ValueRepresentation {
		todo!()
	}

	fn get_type_name(&self) -> &str {
		match self {
			VirtualNodeReference::Direct(d) => d.get_type_name(),
			VirtualNodeReference::CompositeDirect(cd) => cd.get_type_name(),
			VirtualNodeReference::FullRef(fr) => fr.get_type_name(),
			VirtualNodeReference::CompositeFull(cf) => cf.get_type_name(),
			VirtualNodeReference::Wrapping(w) => w.get_type_name(),
		}
	}
}

impl MemoHashed for VirtualNodeReference {
	fn compute_hash_to_memoize(&self) -> HashValue {
		match self {
			VirtualNodeReference::Direct(d) => d.compute_hash_to_memoize(),
			VirtualNodeReference::CompositeDirect(cd) => cd.compute_hash_to_memoize(),
			VirtualNodeReference::FullRef(fr) => fr.compute_hash_to_memoize(),
			VirtualNodeReference::CompositeFull(cf) => cf.compute_hash_to_memoize(),
			VirtualNodeReference::Wrapping(w) => w.compute_hash_to_memoize(),
		}
	}
}

impl VirtualValueExt for VirtualNodeReference {
	fn is_deleted(&self) -> bool {
		match self {
			VirtualNodeReference::Direct(d) => d.is_deleted(),
			VirtualNodeReference::CompositeDirect(cd) => cd.is_deleted(),
			VirtualNodeReference::FullRef(fr) => fr.is_deleted(),
			VirtualNodeReference::CompositeFull(cf) => cf.is_deleted(),
			VirtualNodeReference::Wrapping(w) => w.is_deleted(),
		}
	}
}

impl VirtualNodeValue for VirtualNodeReference {
	fn id(&self) -> Id {
		match self {
			VirtualNodeReference::Direct(d) => d.id(),
			VirtualNodeReference::CompositeDirect(cd) => cd.id(),
			VirtualNodeReference::FullRef(fr) => fr.id(),
			VirtualNodeReference::CompositeFull(cf) => cf.id(),
			VirtualNodeReference::Wrapping(w) => w.id(),
		}
	}
}

impl VirtualNodeReferenceExt for VirtualNodeReference {
	fn element_id(&self) -> ElementId {
		match self {
			VirtualNodeReference::Direct(d) => d.element_id(),
			VirtualNodeReference::CompositeDirect(cd) => cd.element_id(),
			VirtualNodeReference::FullRef(fr) => fr.element_id(),
			VirtualNodeReference::CompositeFull(cf) => cf.element_id(),
			VirtualNodeReference::Wrapping(w) => w.element_id(),
		}
	}
}

//#region ----------------- NODE VALUE -----------------
#[derive(PartialEq, IntoVariants, Display, Measurable)]
pub enum NodeValue {
	#[magic(to_string = "{0}")]
	Direct(DirectNodeValue),
	#[magic(to_string = "{0}")]
	CompositeDirect(CompositeGraphDirectNodeValue),
	#[magic(to_string = "{0}")]
	Wrapping(NodeEntityWrappingNodeValue),
}

impl Hashable for NodeValue {
	fn hash(&self) -> HashValue {
		match self {
			NodeValue::Direct(d) => d.hash(),
			NodeValue::CompositeDirect(cd) => cd.hash(),
			NodeValue::Wrapping(w) => w.hash(),
		}
	}
}

impl AnyValueExt for NodeValue {
	fn write_to<T: AnyValueWriter>(&self, writer: &mut T) -> Result<()> {
		match self {
			NodeValue::Direct(d) => d.write_to(writer),
			NodeValue::CompositeDirect(cd) => cd.write_to(writer),
			NodeValue::Wrapping(w) => w.write_to(writer),
		}
	}

	fn map<T>(&self, mapper: &impl ValueMapper<T>) {
		match self {
			NodeValue::Direct(d) => d.map(mapper),
			NodeValue::CompositeDirect(cd) => cd.map(mapper),
			NodeValue::Wrapping(w) => w.map(mapper),
		}
	}

	fn representation(&self) -> ValueRepresentation {
		todo!()
	}

	fn get_type_name(&self) -> &str {
		match self {
			NodeValue::Direct(d) => d.get_type_name(),
			NodeValue::CompositeDirect(cd) => cd.get_type_name(),
			NodeValue::Wrapping(w) => w.get_type_name(),
		}
	}
}

impl MemoHashed for NodeValue {
	fn compute_hash_to_memoize(&self) -> HashValue {
		match self {
			NodeValue::Direct(d) => d.compute_hash_to_memoize(),
			NodeValue::CompositeDirect(cd) => cd.compute_hash_to_memoize(),
			NodeValue::Wrapping(w) => w.compute_hash_to_memoize(),
		}
	}
}

impl VirtualValueExt for NodeValue {
	fn is_deleted(&self) -> bool {
		match self {
			NodeValue::Direct(d) => d.is_deleted(),
			NodeValue::CompositeDirect(cd) => cd.is_deleted(),
			NodeValue::Wrapping(w) => w.is_deleted(),
		}
	}
}

impl VirtualNodeValue for NodeValue {
	fn id(&self) -> Id {
		match self {
			NodeValue::Direct(d) => d.id(),
			NodeValue::CompositeDirect(cd) => cd.id(),
			NodeValue::Wrapping(w) => w.id(),
		}
	}
}

impl VirtualNodeReferenceExt for NodeValue {
	fn element_id(&self) -> ElementId {
		match self {
			NodeValue::Direct(d) => d.element_id(),
			NodeValue::CompositeDirect(cd) => cd.element_id(),
			NodeValue::Wrapping(w) => w.element_id(),
		}
	}
}

impl NodeValueExt for NodeValue {
	fn labels(&self) -> TextArray {
		match self {
			NodeValue::Direct(d) => d.labels(),
			NodeValue::CompositeDirect(cd) => cd.labels(),
			NodeValue::Wrapping(w) => w.labels(),
		}
	}

	fn properties(&self) -> MapValue {
		match self {
			NodeValue::Direct(d) => d.properties(),
			NodeValue::CompositeDirect(cd) => cd.properties(),
			NodeValue::Wrapping(w) => w.properties(),
		}
	}
}

#[derive(PartialEq, Measurable)]
struct DirectNodeValue {
	id: Id,
	element_id: ElementId,
	#[m(add_m)]
	labels: TextArray,
	#[m(add_m)]
	properties: MapValue,
	is_deleted: bool,
	hash: MemoizedHash,
}

impl DirectNodeValue {
	pub fn new(
		id: Id,
		element_id: ElementId,
		labels: TextArray,
		properties: MapValue,
		is_deleted: bool,
	) -> Self {
		Self {
			id,
			element_id,
			labels,
			properties,
			is_deleted,
			hash: MemoizedHash::new(),
		}
	}
}

impl Display for DirectNodeValue {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		write!(f, "({})", self.id)
	}
}

impl VirtualValueExt for DirectNodeValue {
	fn is_deleted(&self) -> bool {
		self.is_deleted
	}
}

impl VirtualNodeValue for DirectNodeValue {
	fn id(&self) -> Id {
		self.id
	}
}

impl VirtualNodeReferenceExt for DirectNodeValue {
	fn element_id(&self) -> ElementId {
		self.element_id.clone()
	}
}

impl NodeValueExt for DirectNodeValue {
	fn labels(&self) -> TextArray {
		self.labels
	}

	fn properties(&self) -> MapValue {
		self.properties
	}
}

#[derive(PartialEq, Measurable)]
struct CompositeGraphDirectNodeValue {
	id: Id,
	element_id: ElementId,
	source_id: Id,
	#[m(add_m)]
	labels: TextArray,
	#[m(add_m)]
	properties: MapValue,
	is_deleted: bool,
	hash: MemoizedHash,
}

impl CompositeGraphDirectNodeValue {
	pub fn new(
		id: Id,
		element_id: ElementId,
		source_id: Id,
		labels: TextArray,
		properties: MapValue,
		is_deleted: bool,
	) -> Self {
		Self {
			id,
			element_id,
			source_id,
			labels,
			properties,
			is_deleted,
			hash: MemoizedHash::new(),
		}
	}
}

impl CompositeDatabaseValue for CompositeGraphDirectNodeValue {
	fn source_id(&self) -> Id {
		self.source_id
	}
}

impl Display for CompositeGraphDirectNodeValue {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		write!(f, "({})", self.id)
	}
}

impl MemoHashed for CompositeGraphDirectNodeValue {
	fn compute_hash_to_memoize(&self) -> HashValue {
		HASH_CONSTANT * hash_u64(self.id) + hash_u64(self.source_id)
	}
}

impl VirtualValueExt for CompositeGraphDirectNodeValue {
	fn is_deleted(&self) -> bool {
		self.is_deleted
	}
}

impl VirtualNodeValue for CompositeGraphDirectNodeValue {
	fn id(&self) -> Id {
		self.id
	}
}

impl VirtualNodeReferenceExt for CompositeGraphDirectNodeValue {
	fn element_id(&self) -> ElementId {
		self.element_id.clone()
	}
}

impl NodeValueExt for CompositeGraphDirectNodeValue {
	fn labels(&self) -> TextArray {
		self.labels
	}

	fn properties(&self) -> MapValue {
		self.properties
	}
}
//#endregion ----------------- NODE VALUE -----------------

//#region ----------------- NODE REFERENCE -----------------
#[derive(PartialEq, IntoVariants, Display, Measurable)]
pub enum NodeReference {
	#[magic(to_string = "{0}")]
	IdRef(NodeIdReference),
	#[magic(to_string = "{0}")]
	FullRef(FullNodeReference),
	#[magic(to_string = "{0}")]
	CompositeFull(CompositeFullNodeReference),
}

impl Hashable for NodeReference {
	fn hash(&self) -> HashValue {
		match self {
			NodeReference::IdRef(ir) => ir.hash(),
			NodeReference::FullRef(fr) => fr.hash(),
			NodeReference::CompositeFull(cf) => cf.hash(),
		}
	}
}

impl AnyValueExt for NodeReference {
	fn write_to<T: AnyValueWriter>(&self, writer: &mut T) -> Result<()> {
		writer.write_node_reference(self.id())
	}

	fn map<T>(&self, mapper: &impl ValueMapper<T>) {
		mapper.map_node(self);
	}

	fn representation(&self) -> ValueRepresentation {
		match self {
			NodeReference::IdRef(ir) => ir.representation(),
			NodeReference::FullRef(fr) => fr.representation(),
			NodeReference::CompositeFull(cf) => cf.representation(),
		}
	}

	fn get_type_name(&self) -> &str {
		match self {
			NodeReference::IdRef(ir) => ir.get_type_name(),
			NodeReference::FullRef(fr) => fr.get_type_name(),
			NodeReference::CompositeFull(cf) => cf.get_type_name(),
		}
	}
}

impl MemoHashed for NodeReference {
	fn compute_hash_to_memoize(&self) -> HashValue {
		match self {
			NodeReference::IdRef(ir) => ir.compute_hash_to_memoize(),
			NodeReference::FullRef(fr) => fr.compute_hash_to_memoize(),
			NodeReference::CompositeFull(cf) => cf.compute_hash_to_memoize(),
		}
	}
}

impl VirtualValueExt for NodeReference {
	fn is_deleted(&self) -> bool {
		match self {
			NodeReference::IdRef(ir) => ir.is_deleted(),
			NodeReference::FullRef(fr) => fr.is_deleted(),
			NodeReference::CompositeFull(cf) => cf.is_deleted(),
		}
	}
}

impl VirtualNodeValue for NodeReference {
	fn id(&self) -> Id {
		match self {
			NodeReference::IdRef(ir) => ir.id(),
			NodeReference::FullRef(fr) => fr.id(),
			NodeReference::CompositeFull(cf) => cf.id(),
		}
	}
}

#[derive(PartialEq, Measurable)]
struct NodeIdReference {
	id: Id,
	hash: MemoizedHash,
}

impl NodeIdReference {
	pub fn new(id: Id) -> Self {
		Self {
			id,
			hash: MemoizedHash::new(),
		}
	}
}

impl Display for NodeIdReference {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		write!(f, "({})", self.id)
	}
}

impl AnyValueExt for NodeIdReference {
	fn write_to<T: AnyValueWriter>(&self, writer: &mut T) -> Result<()> {
		writer.write_node_reference(self.id)
	}

	fn map<T>(&self, mapper: &impl ValueMapper<T>) {
		mapper.map_node(self);
	}

	fn representation(&self) -> ValueRepresentation {
		todo!()
	}

	fn get_type_name(&self) -> &str {
		"NodeIdReference"
	}
}

impl VirtualValueExt for NodeIdReference {}

impl VirtualNodeValue for NodeIdReference {
	fn id(&self) -> Id {
		self.id
	}
}

#[derive(PartialEq, Measurable)]
struct FullNodeReference {
	id: Id,
	element_id: ElementId,
	hash: MemoizedHash,
}

impl FullNodeReference {
	pub fn new(id: Id, element_id: ElementId) -> FullNodeReference {
		Self {
			id,
			element_id: element_id.to_string(),
			hash: MemoizedHash::new(),
		}
	}

	pub fn new_mapped(id: Id, element_id_mapper: &ElementIdMapper) -> FullNodeReference {
		Self {
			id,
			element_id: element_id_mapper.node_element_id(id),
			hash: MemoizedHash::new(),
		}
	}
}

impl Display for FullNodeReference {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		write!(f, "({})", self.id)
	}
}

impl VirtualValueExt for FullNodeReference {}

impl VirtualNodeValue for FullNodeReference {
	fn id(&self) -> Id {
		self.id
	}
}

impl AnyValueExt for FullNodeReference {
	fn write_to<T: AnyValueWriter>(&self, writer: &mut T) -> Result<()> {
		writer.write_node_reference(self.id)
	}

	fn map<T>(&self, mapper: &impl ValueMapper<T>) {
		mapper.map_node(self);
	}

	fn representation(&self) -> ValueRepresentation {
		todo!()
	}

	fn get_type_name(&self) -> &str {
		"FullNodeReference"
	}
}

impl VirtualNodeReferenceExt for FullNodeReference {
	fn element_id(&self) -> ElementId {
		self.element_id.clone()
	}
}

#[derive(PartialEq, Measurable)]
struct CompositeFullNodeReference {
	id: Id,
	element_id: ElementId,
	source_id: Id,
	hash: MemoizedHash,
}

impl CompositeFullNodeReference {
	pub fn new(id: Id, element_id: ElementId, source_id: Id) -> Self {
		Self {
			id,
			element_id,
			source_id,
			hash: MemoizedHash::new(),
		}
	}
}

impl CompositeDatabaseValue for CompositeFullNodeReference {
	fn source_id(&self) -> Id {
		self.source_id
	}
}

impl MemoHashed for CompositeFullNodeReference {
	fn compute_hash_to_memoize(&self) -> HashValue {
		HASH_CONSTANT * hash_u64(self.id) + hash_u64(self.source_id)
	}
}

impl Display for CompositeFullNodeReference {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		write!(f, "({})", self.id)
	}
}

impl AnyValueExt for CompositeFullNodeReference {
	fn write_to<T: AnyValueWriter>(&self, writer: &mut T) -> Result<()> {
		writer.write_node_reference(self.id)
	}

	fn map<T>(&self, mapper: &impl ValueMapper<T>) {
		mapper.map_node(self);
	}

	fn representation(&self) -> ValueRepresentation {
		todo!()
	}

	fn get_type_name(&self) -> &str {
		"FullNodeReference"
	}
}

impl VirtualValueExt for CompositeFullNodeReference {}

impl VirtualNodeValue for CompositeFullNodeReference {
	fn id(&self) -> Id {
		self.id
	}
}

impl VirtualNodeReferenceExt for CompositeFullNodeReference {
	fn element_id(&self) -> ElementId {
		self.element_id.clone()
	}
}
//#endregion ----------------- NODE REFERENCE -----------------

impl_simple_hashable!(
	DirectNodeValue,
	CompositeGraphDirectNodeValue,
	NodeIdReference,
	FullNodeReference,
	CompositeFullNodeReference
);

impl_id_hash!(
	DirectNodeValue,
	NodeIdReference,
	FullNodeReference
);

impl_any_ext!(
	DirectNodeValue,
	CompositeGraphDirectNodeValue
);
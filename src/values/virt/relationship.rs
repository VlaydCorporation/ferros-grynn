use crate::{impl_id_hash, impl_simple_hashable};
use crate::values::mapper::ValueMapper;
use crate::values::memohash::{HashValue, Hashable, MemoHashed, MemoizedHash, hash_u64, HASH_CONSTANT};
use crate::values::storable::TextValue;
use crate::values::virt::{CompositeDatabaseValue, VirtualValueExt};
use crate::values::virt::node::{VirtualNodeReference, VirtualNodeReferenceExt, VirtualNodeValue};
use crate::values::writer::{AnyValueWriter, EntityMode};
use crate::values::{AnyValueExt, ElementId, Id, ValueRepresentation};
use ferros_grynn_macros::Measurable;
use magic_utils::{Display, IntoVariants};
use std::fmt::{Display, Formatter};
use std::hash::Hash;

macro_rules! impl_any_ext {
    ($($ident:ident),+) => {
		$(
			impl AnyValueExt for $ident {
				fn write_to<T: AnyValueWriter>(&self, writer: &mut T) -> crate::values::error::Result<()> {
					match writer.entity_mode() {
						EntityMode::Reference => writer.write_relationship_reference(self.id()),
						EntityMode::Full => writer.write_relationship(
							self.element_id(),
							self.id(),
							self.start_node_element_id(),
							self.start_node_id(),
							self.end_node_element_id(),
							self.end_node_id(),
							self.get_type(),
							self.properties(),
							self.is_deleted(),
						)
					}
				}

				fn map<T>(&self, mapper: &impl ValueMapper<T>) {
					mapper.map_relationship(self);
				}

				fn representation(&self) -> ValueRepresentation {
					todo!()
				}

				fn get_type_name(&self) -> &str {
					"Relationship"
				}
			}
		)*
	};
}

macro_rules! impl_display {
    ($($ident:ident),+) => {
		$(
			impl Display for $ident {
				fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
					write!(f, "-[{}]-", self.id)
				}
			}
		)*
	};
}

pub const NO_TYPE: i32 = -1;

//#region ----------------- TRAITS -----------------
pub trait RelationshipVisitor {
	fn id(&self) -> Id;
	fn visit(&mut self, start: Id, end: Id, ty: i32);
}

pub trait VirtualRelationshipValueExt: VirtualValueExt {
	fn consume_start_node_id<F>(&mut self, consumer: &F) -> Id
	where
		F: Fn(&mut dyn RelationshipVisitor) -> Id;
	fn consume_end_node_id<F>(&mut self, consumer: &F) -> Id
	where
		F: Fn(&mut dyn RelationshipVisitor) -> Id;
	fn consume_other_node_id<F, V>(&mut self, node: Id, consumer: &F) -> Id
	where
		F: Fn(&mut dyn RelationshipVisitor) -> Id,
	{
		let start = self.consume_start_node_id(consumer);
		if node == start {
			self.consume_end_node_id(consumer)
		} else {
			start
		}
	}

	fn consume_relationship_type_id<F>(&mut self, consumer: &F) -> Option<i32>
	where
		F: Fn(&mut dyn RelationshipVisitor) -> i32;
}

pub trait RelationshipValueExt: VirtualRelationshipValueExt {
	fn element_id(&self) -> ElementId;
	fn get_type(&self) -> TextValue;
	fn properties(&self) -> MapValue;
	fn start_node_id(&self) -> Id;
	fn end_node_id(&self) -> Id;
	fn other_node_id(&self, node: Id) -> Id {
		if node == self.start_node_id() {
			self.end_node_id()
		} else {
			self.start_node_id()
		}
	}
	fn start_node_element_id(&self) -> ElementId {
		self.start_node().element_id()
	}
	fn end_node_element_id(&self) -> ElementId {
		self.end_node().element_id()
	}
	fn start_node(&self) -> VirtualNodeReference;
	fn end_node(&self) -> VirtualNodeReference;
	fn other_node(&self, node: VirtualNodeReference) -> VirtualNodeReference {
		if node == self.start_node() {
			self.end_node()
		} else {
			self.start_node()
		}
	}
}
//#endregion ----------------- TRAITS -----------------

#[derive(PartialEq, IntoVariants, Display, Measurable)]
pub enum VirtualRelationshipValue {
	Reference(RelationshipReference),
	Direct(DirectRelationshipValue),
	CompositeDirect(CompositeDirectRelationshipValue),
}

impl Hashable for VirtualRelationshipValue {
	fn hash(&self) -> HashValue {
		match self {
			VirtualRelationshipValue::Reference(r) => r.hash(),
			VirtualRelationshipValue::Direct(d) => d.hash(),
			VirtualRelationshipValue::CompositeDirect(cd) => cd.hash(),
		}
	}
}

//#region ----------------- RELATIONSHIP VALUE -----------------
#[derive(PartialEq, IntoVariants, Display, Measurable)]
pub enum RelationshipValue {
	#[magic(to_string = "{0}")]
	Direct(DirectRelationshipValue),
	#[magic(to_string = "{0}")]
	CompositeDirect(CompositeDirectRelationshipValue),
	#[magic(to_string = "{0}")]
	Wrapping(RelationshipEntityWrappingValue),
}

impl Hashable for RelationshipValue {
	fn hash(&self) -> HashValue {
		match self {
			RelationshipValue::Direct(d) => d.hash(),
			RelationshipValue::CompositeDirect(cd) => cd.hash(),
			RelationshipValue::Wrapping(w) => w.hash(),
		}
	}
}

impl MemoHashed for RelationshipValue {
	fn compute_hash_to_memoize(&self) -> HashValue {
		match self {
			RelationshipValue::Direct(d) => d.compute_hash_to_memoize(),
			RelationshipValue::CompositeDirect(cd) => cd.compute_hash_to_memoize(),
			RelationshipValue::Wrapping(w) => w.compute_hash_to_memoize(),
		}
	}
}

impl VirtualValueExt for RelationshipValue {
	fn is_deleted(&self) -> bool {
		match self {
			RelationshipValue::Direct(d) => d.is_deleted(),
			RelationshipValue::CompositeDirect(cd) => cd.is_deleted(),
			RelationshipValue::Wrapping(w) => w.is_deleted(),
		}
	}
}

impl RelationshipVisitor for RelationshipValue {
	fn id(&self) -> Id {
		match self {
			RelationshipValue::Direct(d) => d.id(),
			RelationshipValue::CompositeDirect(cd) => cd.id(),
			RelationshipValue::Wrapping(w) => w.id(),
		}
	}

	fn visit(&mut self, start: Id, end: Id, ty: i32) {
		match self {
			RelationshipValue::Direct(d) => {
				unreachable!("DirectRelationshipValue cannot be hydrate!")
			}
			RelationshipValue::CompositeDirect(cd) => {
				unreachable!("CompositeDirectRelationshipValue cannot be hydrate!")
			}
			RelationshipValue::Wrapping(w) => w.visit(start, end, ty),
		}
	}
}

impl VirtualRelationshipValueExt for RelationshipValue {
	fn consume_start_node_id<F>(&mut self, consumer: &F) -> Id
	where
		F: Fn(&mut dyn RelationshipVisitor) -> Id,
	{
		match self {
			RelationshipValue::Direct(d) => d.consume_start_node_id(consumer),
			RelationshipValue::CompositeDirect(cd) => cd.consume_start_node_id(consumer),
			RelationshipValue::Wrapping(w) => w.consume_start_node_id(consumer),
		}
	}

	fn consume_end_node_id<F>(&mut self, consumer: &F) -> Id
	where
		F: Fn(&mut dyn RelationshipVisitor) -> Id,
	{
		match self {
			RelationshipValue::Direct(d) => d.consume_end_node_id(consumer),
			RelationshipValue::CompositeDirect(cd) => cd.consume_end_node_id(consumer),
			RelationshipValue::Wrapping(w) => w.consume_end_node_id(consumer),
		}
	}

	fn consume_relationship_type_id<F>(&mut self, consumer: &F) -> Option<i32>
	where
		F: Fn(&mut dyn RelationshipVisitor) -> i32,
	{
		match self {
			RelationshipValue::Direct(d) => d.consume_relationship_type_id(consumer),
			RelationshipValue::CompositeDirect(cd) => cd.consume_relationship_type_id(consumer),
			RelationshipValue::Wrapping(w) => w.consume_relationship_type_id(consumer),
		}
	}
}

impl RelationshipValueExt for RelationshipValue {
	fn element_id(&self) -> ElementId {
		match self {
			RelationshipValue::Direct(d) => d.element_id(),
			RelationshipValue::CompositeDirect(cd) => cd.element_id(),
			RelationshipValue::Wrapping(w) => w.element_id(),
		}
	}

	fn get_type(&self) -> TextValue {
		match self {
			RelationshipValue::Direct(d) => d.get_type(),
			RelationshipValue::CompositeDirect(cd) => cd.get_type(),
			RelationshipValue::Wrapping(w) => w.get_type(),
		}
	}

	fn properties(&self) -> MapValue {
		match self {
			RelationshipValue::Direct(d) => d.properties(),
			RelationshipValue::CompositeDirect(cd) => cd.properties(),
			RelationshipValue::Wrapping(w) => w.properties(),
		}
	}

	fn start_node_id(&self) -> Id {
		match self {
			RelationshipValue::Direct(d) => d.start_node_id(),
			RelationshipValue::CompositeDirect(cd) => cd.start_node_id(),
			RelationshipValue::Wrapping(w) => w.start_node_id(),
		}
	}

	fn end_node_id(&self) -> Id {
		match self {
			RelationshipValue::Direct(d) => d.end_node_id(),
			RelationshipValue::CompositeDirect(cd) => cd.end_node_id(),
			RelationshipValue::Wrapping(w) => w.end_node_id(),
		}
	}

	fn start_node(&self) -> VirtualNodeReference {
		match self {
			RelationshipValue::Direct(d) => d.start_node(),
			RelationshipValue::CompositeDirect(cd) => cd.start_node(),
			RelationshipValue::Wrapping(w) => w.start_node(),
		}
	}

	fn end_node(&self) -> VirtualNodeReference {
		match self {
			RelationshipValue::Direct(d) => d.end_node(),
			RelationshipValue::CompositeDirect(cd) => cd.end_node(),
			RelationshipValue::Wrapping(w) => w.end_node(),
		}
	}
}

#[derive(PartialEq, Measurable)]
struct DirectRelationshipValue {
	id: Id,
	element_id: ElementId,
	#[m(add_m)]
	start_node: VirtualNodeReference,
	#[m(add_m)]
	end_node: VirtualNodeReference,
	#[m(add_m)]
	ty: TextValue,
	#[m(add_m)]
	properties: MapValue,
	is_deleted: bool,
	hash: MemoizedHash,
}

impl DirectRelationshipValue {
	pub fn new(
		id: Id,
		element_id: ElementId,
		start_node: VirtualNodeReference,
		end_node: VirtualNodeReference,
		ty: TextValue,
		properties: MapValue,
		is_deleted: bool,
	) -> Self {
		Self {
			id,
			element_id,
			start_node,
			end_node,
			ty,
			properties,
			is_deleted,
			hash: MemoizedHash::new(),
		}
	}

	pub fn id(&self) -> Id {
		self.id
	}
}

impl VirtualValueExt for DirectRelationshipValue {
	fn is_deleted(&self) -> bool {
		self.is_deleted
	}
}

impl VirtualRelationshipValueExt for DirectRelationshipValue {
	fn consume_start_node_id<F>(&mut self, _consumer: &F) -> Id
	where
		F: Fn(&mut dyn RelationshipVisitor) -> Id,
	{
		self.start_node.id()
	}

	fn consume_end_node_id<F>(&mut self, _consumer: &F) -> Id
	where
		F: Fn(&mut dyn RelationshipVisitor) -> Id,
	{
		self.end_node.id()
	}

	fn consume_relationship_type_id<F>(&mut self, _consumer: &F) -> Option<i32>
	where
		F: Fn(&mut dyn RelationshipVisitor) -> i32,
	{
		None
	}
}

impl RelationshipValueExt for DirectRelationshipValue {
	fn element_id(&self) -> ElementId {
		self.element_id.clone()
	}

	fn get_type(&self) -> TextValue {
		self.ty.clone()
	}

	fn properties(&self) -> MapValue {
		self.properties
	}

	fn start_node_id(&self) -> Id {
		self.start_node.id()
	}

	fn end_node_id(&self) -> Id {
		self.end_node.id()
	}

	fn start_node(&self) -> VirtualNodeReference {
		self.start_node
	}

	fn end_node(&self) -> VirtualNodeReference {
		self.end_node
	}
}

#[derive(PartialEq, Measurable)]
struct CompositeDirectRelationshipValue {
	id: Id,
	element_id: ElementId,
	source_id: Id,
	#[m(add_m)]
	start_node: VirtualNodeReference,
	#[m(add_m)]
	end_node: VirtualNodeReference,
	#[m(add_m)]
	ty: TextValue,
	#[m(add_m)]
	properties: MapValue,
	is_deleted: bool,
	hash: MemoizedHash,
}

impl CompositeDirectRelationshipValue {
	pub fn new(
		id: Id,
		element_id: ElementId,
		source_id: Id,
		start_node: VirtualNodeReference,
		end_node: VirtualNodeReference,
		ty: TextValue,
		properties: MapValue,
		is_deleted: bool,
	) -> Self {
		Self {
			id,
			element_id,
			source_id,
			start_node,
			end_node,
			ty,
			properties,
			is_deleted,
			hash: MemoizedHash::new(),
		}
	}

	pub fn id(&self) -> Id {
		self.id
	}
}

impl CompositeDatabaseValue for CompositeDirectRelationshipValue {
	fn source_id(&self) -> Id {
		self.source_id
	}
}

impl MemoHashed for CompositeDirectRelationshipValue {
	fn compute_hash_to_memoize(&self) -> HashValue {
		HASH_CONSTANT * hash_u64(self.id) + hash_u64(self.source_id)
	}
}

impl VirtualValueExt for CompositeDirectRelationshipValue {
	fn is_deleted(&self) -> bool {
		self.is_deleted
	}
}

impl VirtualRelationshipValueExt for CompositeDirectRelationshipValue {
	fn consume_start_node_id<F>(&mut self, _consumer: &F) -> Id
	where
		F: Fn(&mut dyn RelationshipVisitor) -> Id,
	{
		self.start_node.id()
	}

	fn consume_end_node_id<F>(&mut self, _consumer: &F) -> Id
	where
		F: Fn(&mut dyn RelationshipVisitor) -> Id,
	{
		self.end_node.id()
	}

	fn consume_relationship_type_id<F>(&mut self, _consumer: &F) -> Option<i32>
	where
		F: Fn(&mut dyn RelationshipVisitor) -> i32,
	{
		None
	}
}

impl RelationshipValueExt for CompositeDirectRelationshipValue {
	fn element_id(&self) -> ElementId {
		self.element_id.clone()
	}

	fn get_type(&self) -> TextValue {
		self.ty
	}

	fn properties(&self) -> MapValue {
		self.properties
	}

	fn start_node_id(&self) -> Id {
		self.start_node.id()
	}

	fn end_node_id(&self) -> Id {
		self.end_node.id()
	}

	fn start_node(&self) -> VirtualNodeReference {
		self.start_node
	}

	fn end_node(&self) -> VirtualNodeReference {
		self.end_node
	}
}
//#endregion ----------------- RELATIONSHIP VALUE -----------------

//#region ----------------- RELATIONSHIP REFERENCE -----------------
#[derive(PartialEq, Measurable)]
pub struct RelationshipReference {
	id: Id,
	start_node: Option<Id>,
	end_node: Option<Id>,
	ty: Option<i32>,
	hash: MemoizedHash,
}

impl RelationshipReference {
	pub fn new(id: Id, start_node: Id, end_node: Id, ty: i32) -> Self {
		Self {
			id,
			start_node: Some(start_node),
			end_node: Some(end_node),
			ty: Some(ty),
			hash: MemoizedHash::new(),
		}
	}

	pub fn with_id(id: Id) -> Self {
		Self {
			id,
			start_node: None,
			end_node: None,
			ty: None,
			hash: MemoizedHash::new(),
		}
	}

	pub fn with_nodes(id: Id, start_node: Id, end_node: Id) -> Self {
		Self {
			id,
			start_node: Some(start_node),
			end_node: Some(end_node),
			ty: None,
			hash: MemoizedHash::new(),
		}
	}
}

impl RelationshipVisitor for RelationshipReference {
	fn id(&self) -> Id {
		self.id
	}

	fn visit(&mut self, start: Id, end: Id, ty: i32) {
		self.start_node = Some(start);
		self.end_node = Some(end);
		self.ty = Some(ty);
	}
}

impl AnyValueExt for RelationshipReference {
	fn write_to<T: AnyValueWriter>(&self, writer: &mut T) -> crate::values::error::Result<()> {
		writer.write_relationship_reference(self.id)
	}

	fn map<T>(&self, mapper: &impl ValueMapper<T>) {
		mapper.map_relationship(self);
	}

	fn representation(&self) -> ValueRepresentation {
		todo!()
	}

	fn get_type_name(&self) -> &str {
		"RelationshipReference"
	}
}

impl VirtualValueExt for RelationshipReference {}

impl VirtualRelationshipValueExt for RelationshipReference {
	fn consume_start_node_id<F>(&mut self, consumer: &F) -> Id
	where
		F: Fn(&mut dyn RelationshipVisitor) -> Id,
	{
		if let Some(start_node) = self.start_node {
			start_node
		} else {
			let id = consumer(self);
			self.start_node = Some(id);
			id
		}
	}

	fn consume_end_node_id<F>(&mut self, consumer: &F) -> Id
	where
		F: Fn(&mut dyn RelationshipVisitor) -> Id,
	{
		if let Some(end_node) = self.end_node {
			end_node
		} else {
			let id = consumer(self);
			self.end_node = Some(id);
			id
		}
	}

	fn consume_relationship_type_id<F>(&mut self, consumer: &F) -> Option<i32>
	where
		F: Fn(&mut dyn RelationshipVisitor) -> i32,
	{
		if let Some(ty) = self.ty {
			Some(ty)
		} else {
			self.ty = Some(consumer(self));
			self.ty
		}
	}
}
//#endregion ----------------- RELATIONSHIP REFERENCE -----------------

impl_simple_hashable!(DirectRelationshipValue, CompositeDirectRelationshipValue, RelationshipReference);
impl_id_hash!(DirectRelationshipValue, RelationshipReference);

impl_display!(DirectRelationshipValue, CompositeDirectRelationshipValue, RelationshipReference);
impl_any_ext!(RelationshipValue, DirectRelationshipValue, CompositeDirectRelationshipValue);

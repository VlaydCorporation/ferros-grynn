use crate::values::error::Result;
use crate::values::mapper::ValueMapper;
use crate::values::memohash::{
	HASH_CONSTANT, HashValue, Hashable, MemoHashed, MemoizedHash, hash_u64,
};
use crate::values::virt::VirtualValueExt;
use crate::values::virt::node::{NodeValue, VirtualNodeValue};
use crate::values::writer::{AnyValueWriter, EntityMode};
use crate::values::{AnyValueExt, Id, ValueRepresentation};
use ferros_grynn_macros::Measurable;
use magic_utils::{Display, IntoVariants};
use std::fmt::{Display, Formatter};
use std::hash::Hash;
use crate::impl_simple_hashable;
use crate::values::virt::list::ListValue;
use crate::values::virt::relationship::RelationshipValue;

macro_rules! impl_hashed {
    ($($ident:ident),+) => {
		$(
			impl MemoHashed for $ident {
				fn compute_hash_to_memoize(&self) -> HashValue {
					let nodes = self.node_ids();
					let relationships = self.relationship_ids();
					let mut result = hash_u64(nodes[0]);
					relationships.iter().zip(nodes[1..].iter()).for_each(|(r, n)| {
						result += HASH_CONSTANT * (result + hash_u64(*r));
						result += HASH_CONSTANT * (result + hash_u64(*n));
					});
					result
				}
			}
		)*
	};
}

//#region ----------------- TRAITS -----------------
pub(crate) trait VirtualPathValue: VirtualValueExt {
	fn size(&self) -> usize;
	fn start_node_id(&self) -> Option<Id>;
	fn end_node_id(&self) -> Option<Id>;
	fn node_ids(&self) -> Vec<Id>;
	fn relationship_ids(&self) -> Vec<Id>;
	fn to_list(&self) -> ListValue;
}

impl<T: VirtualPathValue> VirtualValueExt for T {
	fn is_deleted(&self) -> bool {
		false
	}
}

pub trait PathValueExt: VirtualPathValue {
	fn start_node(&self) -> Option<&NodeValue>;
	fn end_node(&self) -> Option<&NodeValue>;
	fn nodes(&self) -> &[NodeValue];
	fn relationships(&self) -> &[RelationshipValue];
}

impl<P: PathValueExt> AnyValueExt for P {
	fn write_to<T: AnyValueWriter>(&self, writer: &mut T) -> Result<()> {
		if writer.entity_mode() == EntityMode::Reference {
			writer.write_path_ref(self.node_ids().as_slice(), self.relationship_ids().as_slice())
		} else {
			writer.write_path(self.nodes(), self.relationships())
		}
	}

	fn map<T>(&self, mapper: &impl ValueMapper<T>) {
		mapper.map_path(self);
	}

	fn representation(&self) -> ValueRepresentation {
		todo!()
	}

	fn get_type_name(&self) -> &str {
		"Path"
	}
}

impl<P: PathValueExt> VirtualPathValue for P {
	fn size(&self) -> usize {
		self.relationships().len()
	}

	fn start_node_id(&self) -> Option<Id> {
		self.start_node().map(|n| n.id())
	}

	fn end_node_id(&self) -> Option<Id> {
		self.end_node().map(|n| n.id())
	}

	fn node_ids(&self) -> Vec<Id> {
		self.nodes().into_iter().map(|v| v.id()).collect()
	}

	fn relationship_ids(&self) -> Vec<Id> {
		self.relationships().into_iter().map(|v| v.id()).collect()
	}

	fn to_list(&self) -> ListValue {
		let nodes = self.nodes();
		let relationships = self.relationships();
		let size = nodes.len() + relationships.len();
		let builder = ListValueBuilder::new(size);
		for i in 0..size {
			if i % 2 == 0 {
				builder.add(nodes[i / 2]);
			} else {
				builder.add(relationships[i / 2]);
			}
		}
		builder.build()
	}
}
//#endregion ----------------- TRAITS -----------------

//#region ----------------- PATH VALUE -----------------
// TODO: maybe hide implementation with private inner field
#[derive(PartialEq, IntoVariants, Display, Measurable)]
pub enum PathValue {
	#[magic(to_string = "{0}")]
	Direct(DirectPathValue),
	#[magic(to_string = "{0}")]
	Wrapping(PathEntityWrappingPathValue),
}

impl Hashable for PathValue {
	fn hash(&self) -> HashValue {
		match self {
			PathValue::Direct(d) => d.hash(),
			PathValue::Wrapping(w) => w.hash(),
		}
	}
}

impl PathValueExt for PathValue {
	fn start_node(&self) -> Option<&NodeValue> {
		match self {
			PathValue::Direct(d) => d.start_node(),
			PathValue::Wrapping(w) => w.start_node(),
		}
	}

	fn end_node(&self) -> Option<&NodeValue> {
		match self {
			PathValue::Direct(d) => d.end_node(),
			PathValue::Wrapping(w) => w.end_node(),
		}
	}

	fn nodes(&self) -> &[NodeValue] {
		match self {
			PathValue::Direct(d) => d.nodes(),
			PathValue::Wrapping(w) => w.nodes(),
		}
	}

	fn relationships(&self) -> &[RelationshipValue] {
		match self {
			PathValue::Direct(d) => d.relationships(),
			PathValue::Wrapping(w) => w.relationships(),
		}
	}
}

#[derive(PartialEq, Measurable)]
struct DirectPathValue {
	nodes: Vec<NodeValue>,
	edges: Vec<RelationshipValue>,
	#[m(add)]
	payload_size: usize,
	hash: MemoizedHash,
}

impl DirectPathValue {
	pub fn new(nodes: &[NodeValue], edges: &[RelationshipValue], payload_size: usize) -> Self {
		// TODO:
		if nodes.len() != edges.len() + 1 {
			panic!("nodes.len() != edges.len() + 1")
		}

		DirectPathValue {
			nodes: nodes.to_vec(),
			edges: edges.to_vec(),
			payload_size,
			hash: MemoizedHash::new(),
		}
	}
}

impl Display for DirectPathValue {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}{{", self.get_type_name())?;

		let mut i = 0;
		while i < self.edges.len() {
			write!(f, "{}{}", self.nodes[i], self.edges[i])?;
			i += 1;
		}
		write!(f, "{}", self.nodes[i])?;

		write!(f, "}}")
	}
}

impl PathValueExt for DirectPathValue {
	fn start_node(&self) -> Option<&NodeValue> {
		self.nodes.first()
	}

	fn end_node(&self) -> Option<&NodeValue> {
		self.nodes.last()
	}

	fn nodes(&self) -> &[NodeValue] {
		self.nodes.as_slice()
	}

	fn relationships(&self) -> &[RelationshipValue] {
		self.edges.as_slice()
	}
}
//#endregion ----------------- PATH VALUE -----------------

//#region ----------------- PATH REFERENCE -----------------
// TODO: maybe hide implementation with private inner field
#[derive(PartialEq, IntoVariants, Display, Measurable)]
pub enum PathReference {
	#[magic(to_string = "{0}")]
	Primitive(PathReferencePrimitive),
	#[magic(to_string = "{0}")]
	References(PathReferenceReferences),
}

impl Hashable for PathReference {
	fn hash(&self) -> HashValue {
		match self {
			PathReference::Primitive(p) => p.hash(),
			PathReference::References(r) => r.hash(),
		}
	}
}

impl PathReference {
	pub fn primitive_path(nodes: &[Id], relationships: &[Id]) -> Self {
		Self::Primitive(PathReferencePrimitive::new(nodes, relationships))
	}

	pub fn references_path(nodes: &[NodeValue], relationships: &[RelationshipValue]) -> Self {
		Self::References(PathReferenceReferences::new(nodes, relationships))
	}

	pub fn relationships_list(&self) -> ListValue {
		match self {
			PathReference::Primitive(p) => p.relationships_list(),
			PathReference::References(r) => r.relationships_list(),
		}
	}
}

impl AnyValueExt for PathReference {
	fn write_to<T: AnyValueWriter>(&self, writer: &mut T) -> Result<()> {
		match self {
			PathReference::Primitive(p) => p.write_to(writer),
			PathReference::References(r) => r.write_to(writer),
		}
	}

	fn map<T>(&self, mapper: &impl ValueMapper<T>) {
		mapper.map_path(self);
	}

	fn representation(&self) -> ValueRepresentation {
		todo!()
	}

	fn get_type_name(&self) -> &str {
		"Path"
	}
}

impl VirtualPathValue for PathReference {
	fn size(&self) -> usize {
		match self {
			PathReference::Primitive(p) => p.size(),
			PathReference::References(r) => r.size(),
		}
	}

	fn start_node_id(&self) -> Option<Id> {
		match self {
			PathReference::Primitive(p) => p.start_node_id(),
			PathReference::References(r) => r.start_node_id(),
		}
	}

	fn end_node_id(&self) -> Option<Id> {
		match self {
			PathReference::Primitive(p) => p.end_node_id(),
			PathReference::References(r) => r.end_node_id(),
		}
	}

	fn node_ids(&self) -> Vec<Id> {
		match self {
			PathReference::Primitive(p) => p.node_ids(),
			PathReference::References(r) => r.node_ids(),
		}
	}

	fn relationship_ids(&self) -> Vec<Id> {
		match self {
			PathReference::Primitive(p) => p.relationship_ids(),
			PathReference::References(r) => r.relationship_ids(),
		}
	}

	fn to_list(&self) -> ListValue {
		match self {
			PathReference::Primitive(p) => p.to_list(),
			PathReference::References(r) => r.to_list(),
		}
	}
}

#[derive(PartialEq, Measurable)]
struct PathReferencePrimitive {
	nodes: Vec<Id>,
	relationships: Vec<Id>,
	hash: MemoizedHash,
}

impl PathReferencePrimitive {
	pub fn new(nodes: &[Id], relationships: &[Id]) -> Self {
		Self {
			nodes: nodes.to_vec(),
			relationships: relationships.to_vec(),
			hash: MemoizedHash::new(),
		}
	}

	pub fn relationships_list(&self) -> ListValue {
		let builder = ListValueBuilder::new(self.relationships.len());
		for relationship in &self.relationships {
			builder.add(utils::relationship_by_id(relationship));
		}
		builder.build()
	}
}

impl AnyValueExt for PathReferencePrimitive {
	fn write_to<T: AnyValueWriter>(&self, writer: &mut T) -> Result<()> {
		writer.write_path_ref(&self.nodes, &self.relationships)
	}

	fn map<T>(&self, mapper: &impl ValueMapper<T>) {
		mapper.map_path(self);
	}

	fn representation(&self) -> ValueRepresentation {
		todo!()
	}

	fn get_type_name(&self) -> &str {
		"Path"
	}
}

impl VirtualPathValue for PathReferencePrimitive {
	fn size(&self) -> usize {
		self.relationships.len()
	}

	fn start_node_id(&self) -> Option<Id> {
		self.nodes.first().cloned()
	}

	fn end_node_id(&self) -> Option<Id> {
		self.nodes.last().cloned()
	}

	fn node_ids(&self) -> Vec<Id> {
		self.nodes.clone()
	}

	fn relationship_ids(&self) -> Vec<Id> {
		self.relationships.clone()
	}

	fn to_list(&self) -> ListValue {
		let size = self.nodes.len() + self.relationships.len();
		let builder = ListValueBuilder::new(size);
		for i in 0..size {
			if i % 2 == 0 {
				builder.add(utils::node_by_id(self.nodes[i / 2]));
			} else {
				builder.add(utils::relationship_by_id(self.relationships[i / 2]));
			}
		}
		builder.build()
	}
}

impl Display for PathReferencePrimitive {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}{{", self.get_type_name())?;

		write!(f, "({})", self.nodes[0])?;
		let mut i = 0;
		while i < self.relationships.len() {
			write!(f, "-[{}]-({})", self.relationships[i], self.nodes[i + 1])?;
			i += 1;
		}

		write!(f, "}}")
	}
}

#[derive(PartialEq, Measurable)]
struct PathReferenceReferences {
	nodes: Vec<NodeValue>,
	relationships: Vec<RelationshipValue>,
	hash: MemoizedHash,
}

impl PathReferenceReferences {
	pub fn new(nodes: &[NodeValue], relationships: &[RelationshipValue]) -> Self {
		Self {
			nodes: nodes.to_vec(),
			relationships: relationships.to_vec(),
			hash: MemoizedHash::new(),
		}
	}
}

impl AnyValueExt for PathReferenceReferences {
	fn write_to<T: AnyValueWriter>(&self, writer: &mut T) -> Result<()> {
		writer.write_path_ref_v(self.nodes, self.relationships)
	}

	fn map<T>(&self, mapper: &impl ValueMapper<T>) {
		mapper.map_path(self);
	}

	fn representation(&self) -> ValueRepresentation {
		todo!()
	}

	fn get_type_name(&self) -> &str {
		"Path"
	}
}

impl VirtualPathValue for PathReferenceReferences {
	fn size(&self) -> usize {
		self.relationships.len()
	}

	fn start_node_id(&self) -> Option<Id> {
		self.nodes.first().map(|n| n.id())
	}

	fn end_node_id(&self) -> Option<Id> {
		self.nodes.last().map(|n| n.id())
	}

	fn node_ids(&self) -> Vec<Id> {
		self.nodes.iter().map(|n| n.id()).collect()
	}

	fn relationship_ids(&self) -> Vec<Id> {
		self.relationships.iter().map(|n| n.id())
	}

	fn to_list(&self) -> ListValue {
		let size = self.nodes.len() + self.relationships.len();
		let builder = ListValueBuilder::new(size);
		for i in 0..size {
			if i % 2 == 0 {
				builder.add(self.nodes[i / 2]);
			} else {
				builder.add(self.relationships[i / 2]);
			}
		}
		builder.build()
	}
}

impl Display for PathReferenceReferences {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}{{", self.get_type_name())?;

		write!(f, "({})", self.nodes[0].id())?;
		let mut i = 0;
		while i < self.relationships.len() {
			write!(f, "-[{}]-({})", self.relationships[i].id(), self.nodes[i + 1].id())?;
			i += 1;
		}

		write!(f, "}}")
	}
}
//#endregion ----------------- PATH REFERENCE -----------------

impl_hashed!(
	DirectPathValue,
	PathReferencePrimitive,
	PathReferenceReferences
);
impl_simple_hashable!(
	DirectPathValue,
	PathReferencePrimitive,
	PathReferenceReferences
);

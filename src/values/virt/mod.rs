use crate::values::memohash::MemoHashed;
use crate::values::{AnyValueExt, Id};

mod list;
mod map;
pub mod node;
pub mod path;
mod relationship;

pub use list::{ListValue, ListValueBuilder};
pub use map::MapValue;
pub use node::{NodeValue, NodeReference, VirtualNodeReference};
pub use path::{PathValue, PathReference};
pub use relationship::{RelationshipValue, RelationshipReference, VirtualRelationshipValue};

pub trait VirtualValueExt: AnyValueExt + MemoHashed {
	fn is_deleted(&self) -> bool {
		false
	}
}

pub trait CompositeDatabaseValue {
	fn source_id(&self) -> Id;
}

#[derive(PartialEq)]
pub enum VirtualValue {
	Node(NodeValue),
	NodeReference(NodeReference),

	PathValue(PathValue),
	PathReference(PathReference),

	Relationship(RelationshipValue),
	RelationshipReference(RelationshipReference),

	Map(MapValue),
	List(ListValue),
	// Error(error::ErrorValue)
}

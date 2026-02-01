use crate::common::memory::Measurable;
use crate::unsafed::shallow_size_of;
use crate::values::Equality;
use crate::values::storable::{Value};
use crate::values::mapper::ValueMapper;
use crate::values::writer::AnyValueWriter;

#[derive(Debug)]
pub struct GraphReferenceValue {
	db_ref: DatabaseReference,
}

impl GraphReferenceValue {
	pub fn new(db_ref: DatabaseReference) -> GraphReferenceValue {
		GraphReferenceValue {
			db_ref,
		}
	}

	pub fn db_ref(&self) -> &DatabaseReference {
		self.db_ref
	}
}

impl Measurable for GraphReferenceValue {
	fn estimated_heap_usage(&self) -> usize {
		shallow_size_of::<GraphReferenceValue>()
	}
}

impl PartialEq for GraphReferenceValue {
	fn eq(&self, other: &Self) -> bool {
		self.db_ref == other.db_ref
	}
}

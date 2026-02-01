use crate::common::memory::Measurable;
use crate::values::error::Result;
use crate::values::graph_reference::GraphReferenceValue;
use crate::values::mapper::ValueMapper;
use crate::values::memohash::Hashable;
use crate::values::representation::ValueRepresentation;
use crate::values::storable::Value;
use crate::values::virt::VirtualValue;
use crate::values::writer::AnyValueWriter;
use magic_utils::{Display, IntoVariants};
use std::fmt::Display;
use std::hash::Hash;

mod error;
mod graph_reference;
mod mapper;
mod memohash;
mod representation;
mod sequence;
mod storable;
mod virt;
mod writer;
mod utils;

pub use virt::*;

pub type Id = u64;
pub type ElementId = String;

pub trait AnyValueExt: Measurable + Hashable + PartialEq + Display {
	fn write_to<T: AnyValueWriter>(&self, writer: &mut T) -> Result<()>;
	fn map<T>(&self, mapper: &impl ValueMapper<T>);
	fn representation(&self) -> ValueRepresentation;
	fn get_type_name(&self) -> &str;
}

#[derive(Default, PartialEq, Display, IntoVariants)]
pub enum AnyValue {
	#[default]
	NoValue,
	GraphReference(GraphReferenceValue),
	Value(Value),
	Virtual(VirtualValue),
}

impl AnyValue {
	pub fn is_no_value(&self) -> bool {
		matches!(self, AnyValue::NoValue)
	}

	pub fn is_nan(&self) -> bool {
		matches!(self, AnyValue::Value(v) if v.is_nan())
	}

	pub fn ternary_equals(&self, other: &Self) -> Equality {
		if self.is_nan() || other.is_nan() {
			return Equality::False;
		}
		if let (Value::Null, _) | (_, Value::Null) = (self, other) {
			return Equality::Undefined;
		}
		if self == other {
			Equality::True
		} else {
			Equality::False
		}
	}

	fn eq_with_null_check(&self, other: &AnyValue) -> bool {
		match (self, other) {
			(AnyValue::Value(lhs), AnyValue::Value(rhs))
				if matches!(lhs, Value::Null) || matches!(rhs, Value::Null) =>
			{
				false
			}
			_ => self == other,
		}
	}

	fn is_sequence(&self) -> bool {
		match self {
			AnyValue::Value(v) => v.is_sequence(),
			AnyValue::Virtual(vv) => vv.is_sequence(),
			_ => false,
		}
	}

	fn is_incomparable(&self) -> bool {
		match self {
			AnyValue::Value(v) => v.is_incomparable(),
			_ => false,
		}
	}
}

impl Measurable for AnyValue {
	fn estimated_heap_usage(&self) -> usize {
		match self {
			AnyValue::GraphReference(gr) => gr.estimated_heap_usage(),
			AnyValue::Value(v) => v.estimated_heap_usage(),
			AnyValue::Virtual(vv) => vv.estimated_heap_usage(),
		}
	}
}

impl AnyValueExt for AnyValue {
	fn write_to<T: AnyValueWriter>(&self, writer: &mut T) -> Result<()> {
		match self {
			AnyValue::GraphReference(_) => panic!("GraphReferenceValue.writeTo not implemented"),
			AnyValue::Value(v) => v.write_to(writer),
			AnyValue::Virtual(vv) => vv.write_to(writer),
		}
	}

	fn map<T>(&self, mapper: &impl ValueMapper<T>) {
		match self {
			AnyValue::GraphReference(_) => panic!("GraphReferenceValue.writeTo not implemented"),
			AnyValue::Value(v) => v.map(mapper),
			AnyValue::Virtual(vv) => vv.map(mapper),
		}
	}

	fn representation(&self) -> ValueRepresentation {
		todo!()
	}

	fn get_type_name(&self) -> &str {
		match self {
			AnyValue::GraphReference(g) => g.get_type_name(),
			AnyValue::Value(v) => v.get_type_name(),
			AnyValue::Virtual(vv) => vv.get_type_name(),
		}
	}
}

pub enum ArrayType {
	Int,
	Float,
	Bool,
	String,
	ZonedDateTime,
	LocalDateTime,
	Date,
	ZonedTime,
	LocalTime,
	Duration,
}

pub enum Equality {
	True,
	False,
	Undefined,
}

mod array;
mod bytes;
mod closure;
mod conversions;
mod error_value;
mod formatting;
mod geometry;
mod id;
mod kind;
mod number;
mod object;
mod property_dictionary;
mod property_map;
mod query_cursor;
mod range;
mod record_id;
mod regex;
mod set;
mod temporal;
mod text;
mod types;
mod value;
mod vector;

#[cfg(test)]
mod laws;

pub use array::Array;
pub use bytes::Bytes;
pub use closure::{Closure, RuntimeId};
pub use error_value::ErrorValue;
pub use geometry::{Geometry, Position};
pub use graph::{
	AtomKind,
	AtomOccurrenceRef,
	AtomRef,
	Frozen,
	FrozenGraph,
	Graph,
	GraphId,
	GraphKind,
	GraphRef,
	GraphStorageMode,
	MaterializedGraph,
	Mutable,
	MutableGraph,
	Path,
};
pub use id::{Ulid, Uuid};
pub use kind::ValueKind;
pub use number::Number;
pub use object::Object;
pub use property_dictionary::PropertyDictionary;
pub use property_map::{PropertyIter, PropertyKeyId, PropertyMap};
pub use query_cursor::QueryCursor;
pub use range::Range;
pub use record_id::{RecordId, RecordIdPart};
pub use regex::Regex;
pub use set::Set;
pub use temporal::{DateTime, Duration};
pub use text::Text;
pub use types::Type;
pub use value::Value;
pub use vector::{Vector, VectorKind};

pub use crate::graph;

#[cfg(test)]
mod tests {
	use std::{
		collections::{BTreeSet, HashSet},
		sync::Arc,
	};

	use rust_decimal::Decimal;

	use super::{Number, Text, Type, Value, ValueKind};
	use crate::values::runtime::graph::{
		AtomKind,
		AtomOccurrenceRef,
		AtomRef,
		Graph,
		GraphId,
		GraphKind,
		GraphRef,
		GraphStorageMode,
		Mutable,
		Path,
		SnapshotId,
		Subgraph,
		Tree,
		TreeOrientation,
	};

	#[test]
	fn sentinel_semantics_are_two_valued() {
		assert_eq!(Value::None, Value::None);
		assert_eq!(Value::Null, Value::Null);
		assert_ne!(Value::None, Value::Null);
		assert_ne!(Value::Null, Value::from(1_i64));
		assert!(!Value::None.is_truthy());
		assert!(!Value::Null.is_truthy());
	}

	#[test]
	#[allow(clippy::mutable_key_type)]
	fn uses_explicit_stable_variant_order() {
		let values = BTreeSet::from([
			Value::from("string"),
			Value::Null,
			Value::from(true),
			Value::None,
			Value::from(1_i64),
		]);
		assert_eq!(values.into_iter().map(|value| value.kind()).collect::<Vec<_>>(), vec![
			ValueKind::None,
			ValueKind::Null,
			ValueKind::Bool,
			ValueKind::Int,
			ValueKind::String,
		],);
	}

	#[test]
	#[allow(clippy::mutable_key_type)]
	fn eq_ord_and_hash_agree_for_cross_number_values() {
		let values = HashSet::from([
			Value::from(Number::Int(1)),
			Value::from(Number::Float(1.0)),
			Value::from(Number::Decimal(Decimal::ONE)),
		]);
		assert_eq!(values.len(), 1);
	}

	#[test]
	fn reports_concrete_kind_and_static_type() {
		let value = Value::from(1.5_f64);
		assert_eq!(value.kind(), ValueKind::Float);
		assert_eq!(value.value_type(), Type::Float);
	}

	#[test]
	fn display_is_deterministic_and_escapes_strings() {
		let value = Value::from("a\0\nb");
		assert_eq!(value.to_string(), r#""a\u0000\nb""#);
	}

	#[test]
	fn runtime_only_values_are_explicitly_non_storable() {
		let closure = super::Closure::new(None, vec![], Type::None, |_| Ok(Value::None));
		let value = Value::from(vec![Value::from(closure)]);
		assert!(!value.is_storable());
		assert!(value.ensure_storable().is_err());
	}

	#[test]
	fn value_conversions_never_default_or_truncate() {
		assert_eq!(i64::try_from(Value::from(42_i64)).unwrap(), 42);
		assert!(i64::try_from(Value::from(42.5_f64)).is_err());
		assert!(bool::try_from(Value::Null).is_err());

		let value = Value::from("graph");
		assert_eq!(value.as_text(), Some(&Text::from("graph")));
		assert_eq!(Text::try_from(value).unwrap(), "graph");
	}

	#[test]
	fn value_respects_runtime_size_budget() {
		let size = std::mem::size_of::<Value>();
		assert!(
			size <= 64,
			"Value={size}, DateTime={}, RecordId={}, Geometry={}, Closure={}",
			std::mem::size_of::<super::DateTime>(),
			std::mem::size_of::<super::RecordId>(),
			std::mem::size_of::<super::Geometry>(),
			std::mem::size_of::<super::Closure>(),
		);
	}

	#[test]
	fn graph_transport_preserves_exact_type_and_typed_conversion() {
		let tree =
			Tree::<Mutable>::new(TreeOrientation::OutArborescence, None, super::PropertyMap::new())
				.unwrap()
				.freeze();
		let value = Value::from(tree);
		assert_eq!(value.kind(), ValueKind::Graph);
		assert_eq!(value.value_type(), Type::Graph(GraphKind::Tree));
		assert!(Tree::try_from(value).is_ok());

		let general = Value::from(Graph::<Mutable>::new().freeze());
		assert!(Tree::try_from(general).is_err());

		let subgraph = Value::from(Subgraph::<Mutable>::new().freeze());
		assert_eq!(subgraph.kind(), ValueKind::Subgraph);
		assert_eq!(subgraph.value_type(), Type::Graph(GraphKind::Subgraph));
	}

	#[test]
	fn atom_and_path_have_distinct_value_contracts() {
		let atom = AtomRef::new(AtomKind::Node, "person:1".parse().unwrap());
		let atom_value = Value::from(atom.clone());
		assert_eq!(atom_value.kind(), ValueKind::Atom);
		assert_eq!(atom_value.value_type(), Type::Atom(AtomKind::Node));
		assert_eq!(AtomRef::try_from(atom_value).unwrap(), atom);

		let path = Path::new(super::Uuid::default(), SnapshotId::new(7, 11), Vec::new(), None);
		let path_value = Value::from(path.clone());
		assert_eq!(path_value.kind(), ValueKind::Path);
		assert_eq!(path_value.value_type(), Type::Path);
		assert_eq!(Path::try_from(path_value).unwrap(), path);
	}

	#[test]
	fn occurrence_identity_uses_graph_id_and_atom_identity() {
		let atom = AtomRef::new(AtomKind::Node, "person:1".parse().unwrap());
		let graph = Arc::new(
			GraphRef::new(
				GraphId::new(7).unwrap(),
				"first",
				GraphKind::Graph,
				GraphStorageMode::InMemory,
			)
			.unwrap(),
		);
		let renamed = Arc::new(
			GraphRef::new(
				GraphId::new(7).unwrap(),
				"renamed",
				GraphKind::Dag,
				GraphStorageMode::DiskBacked,
			)
			.unwrap(),
		);
		let other = Arc::new(
			GraphRef::new(
				GraphId::new(8).unwrap(),
				"other",
				GraphKind::Graph,
				GraphStorageMode::InMemory,
			)
			.unwrap(),
		);

		let first = AtomOccurrenceRef::new(atom.clone(), graph);
		let same_identity = AtomOccurrenceRef::new(atom.clone(), renamed);
		let other_graph = AtomOccurrenceRef::new(atom, other);
		assert_eq!(first, same_identity);
		assert_ne!(first, other_graph);
		assert_eq!(HashSet::from([first.clone(), same_identity]).len(), 1);

		let value = Value::from(first);
		assert_eq!(value.kind(), ValueKind::AtomOccurrence);
		assert_eq!(value.value_type(), Type::AtomOccurrence(AtomKind::Node));
		assert!(!value.is_storable());
	}
}

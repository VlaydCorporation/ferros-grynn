use super::{
	Array,
	AtomOccurrenceRef,
	AtomRef,
	Bytes,
	Closure,
	DateTime,
	Duration,
	ErrorValue,
	Frozen,
	Geometry,
	Graph,
	GraphRef,
	MaterializedGraph,
	Number,
	Object,
	Path,
	QueryCursor,
	Range,
	RecordId,
	Regex,
	Set,
	Text,
	Ulid,
	Uuid,
	Value,
	Vector,
	graph,
};
use crate::gql_status::FerrosGrynnError;

impl From<bool> for Value {
	fn from(value: bool) -> Self {
		Self::Bool(value)
	}
}

macro_rules! impl_number_value {
	($($type:ty),* $(,)?) => {
		$(
			impl From<$type> for Value {
				fn from(value: $type) -> Self {
					Self::Number(Number::from(value))
				}
			}
		)*
	};
}

impl_number_value!(
	i8,
	i16,
	i32,
	i64,
	isize,
	u8,
	u16,
	u32,
	u64,
	usize,
	f32,
	f64,
	rust_decimal::Decimal,
);

impl From<Number> for Value {
	fn from(value: Number) -> Self {
		Self::Number(value)
	}
}

impl From<Text> for Value {
	fn from(value: Text) -> Self {
		Self::String(value)
	}
}

impl From<String> for Value {
	fn from(value: String) -> Self {
		Self::String(Text::from(value))
	}
}

impl From<&str> for Value {
	fn from(value: &str) -> Self {
		Self::String(Text::from(value))
	}
}

impl From<char> for Value {
	fn from(value: char) -> Self {
		Self::String(Text::from(value))
	}
}

macro_rules! impl_value_variant {
	($($type:ty => $variant:ident),* $(,)?) => {
		$(
			impl From<$type> for Value {
				fn from(value: $type) -> Self {
					Self::$variant(value)
				}
			}
		)*
	};
}

impl_value_variant!(
	Array => Array,
	Bytes => Bytes,
	DateTime => DateTime,
	Duration => Duration,
	Geometry => Geometry,
	Object => Object,
	Range => Range,
	Regex => Regex,
	Set => Set,
	Ulid => Ulid,
	Uuid => Uuid,
	Vector => Vector,
);

impl From<Vec<Value>> for Value {
	fn from(value: Vec<Value>) -> Self {
		Self::Array(Array::new(value))
	}
}

impl From<Closure> for Value {
	fn from(value: Closure) -> Self {
		Self::Closure(Box::new(value))
	}
}

impl From<RecordId> for Value {
	fn from(value: RecordId) -> Self {
		Self::Record(Box::new(value))
	}
}

impl From<ErrorValue> for Value {
	fn from(value: ErrorValue) -> Self {
		Self::Error(Box::new(value))
	}
}

impl From<QueryCursor> for Value {
	fn from(value: QueryCursor) -> Self {
		Self::Cursor(value)
	}
}

impl From<GraphRef> for Value {
	fn from(value: GraphRef) -> Self {
		Self::GraphRef(Box::new(value))
	}
}

impl From<AtomRef> for Value {
	fn from(value: AtomRef) -> Self {
		Self::Atom(Box::new(value))
	}
}

impl From<Path> for Value {
	fn from(value: Path) -> Self {
		Self::Path(Box::new(value))
	}
}

impl From<MaterializedGraph> for Value {
	fn from(value: MaterializedGraph) -> Self {
		Self::Graph(Box::new(value))
	}
}

macro_rules! impl_graph_value {
	($($type:ty),* $(,)?) => {
		$(
			impl From<$type> for Value {
				fn from(value: $type) -> Self {
					Self::from(MaterializedGraph::from(value))
				}
			}
		)*
	};
}

impl_graph_value!(
	Graph<Frozen>,
	graph::Tree<Frozen>,
	graph::RootedTree<Frozen>,
	graph::Forest<Frozen>,
	graph::Dag<Frozen>,
	graph::Hypergraph<Frozen>,
	graph::Metagraph<Frozen>,
	graph::Subgraph<Frozen>,
);

fn value_conversion_error(value: &Value, target: &'static str) -> FerrosGrynnError {
	FerrosGrynnError::conversion_error(value.kind().name(), target)
}

impl TryFrom<Value> for bool {
	type Error = FerrosGrynnError;

	fn try_from(value: Value) -> Result<Self, Self::Error> {
		match value {
			Value::Bool(value) => Ok(value),
			value => Err(value_conversion_error(&value, "bool")),
		}
	}
}

impl TryFrom<Value> for Number {
	type Error = FerrosGrynnError;

	fn try_from(value: Value) -> Result<Self, Self::Error> {
		match value {
			Value::Number(value) => Ok(value),
			value => Err(value_conversion_error(&value, "Number")),
		}
	}
}

macro_rules! try_number_from_value {
	($($type:ty),* $(,)?) => {
		$(
			impl TryFrom<Value> for $type {
				type Error = FerrosGrynnError;

				fn try_from(value: Value) -> Result<Self, Self::Error> {
					match value {
						Value::Number(value) => Self::try_from(value),
						value => Err(value_conversion_error(&value, stringify!($type))),
					}
				}
			}
		)*
	};
}

try_number_from_value!(
	i8,
	i16,
	i32,
	i64,
	i128,
	isize,
	u8,
	u16,
	u32,
	u64,
	u128,
	usize,
	f32,
	f64,
	rust_decimal::Decimal,
);

impl TryFrom<Value> for Text {
	type Error = FerrosGrynnError;

	fn try_from(value: Value) -> Result<Self, Self::Error> {
		match value {
			Value::String(value) => Ok(value),
			value => Err(value_conversion_error(&value, "Text")),
		}
	}
}

impl TryFrom<Value> for String {
	type Error = FerrosGrynnError;

	fn try_from(value: Value) -> Result<Self, Self::Error> {
		Text::try_from(value).map(Text::into_string)
	}
}

macro_rules! try_value_variant {
	($($type:ty => $variant:ident),* $(,)?) => {
		$(
			impl TryFrom<Value> for $type {
				type Error = FerrosGrynnError;

				fn try_from(value: Value) -> Result<Self, Self::Error> {
					match value {
						Value::$variant(value) => Ok(value),
						value => Err(value_conversion_error(&value, stringify!($type))),
					}
				}
			}
		)*
	};
}

try_value_variant!(
	Array => Array,
	Bytes => Bytes,
	DateTime => DateTime,
	Duration => Duration,
	Geometry => Geometry,
	Object => Object,
	Range => Range,
	Regex => Regex,
	Set => Set,
	Ulid => Ulid,
	Uuid => Uuid,
	Vector => Vector,
);

impl TryFrom<Value> for Closure {
	type Error = FerrosGrynnError;

	fn try_from(value: Value) -> Result<Self, Self::Error> {
		match value {
			Value::Closure(value) => Ok(*value),
			value => Err(value_conversion_error(&value, "Closure")),
		}
	}
}

impl TryFrom<Value> for RecordId {
	type Error = FerrosGrynnError;

	fn try_from(value: Value) -> Result<Self, Self::Error> {
		match value {
			Value::Record(value) => Ok(*value),
			value => Err(value_conversion_error(&value, "RecordId")),
		}
	}
}

impl From<AtomOccurrenceRef> for Value {
	fn from(value: AtomOccurrenceRef) -> Self {
		Self::AtomOccurrence(Box::new(value))
	}
}

impl TryFrom<Value> for GraphRef {
	type Error = FerrosGrynnError;

	fn try_from(value: Value) -> Result<Self, Self::Error> {
		match value {
			Value::GraphRef(value) => Ok(*value),
			value => Err(value_conversion_error(&value, "GraphRef")),
		}
	}
}

impl TryFrom<Value> for AtomRef {
	type Error = FerrosGrynnError;

	fn try_from(value: Value) -> Result<Self, Self::Error> {
		match value {
			Value::Atom(value) => Ok(*value),
			value => Err(value_conversion_error(&value, "AtomRef")),
		}
	}
}

impl TryFrom<Value> for AtomOccurrenceRef {
	type Error = FerrosGrynnError;

	fn try_from(value: Value) -> Result<Self, Self::Error> {
		match value {
			Value::AtomOccurrence(value) => Ok(*value),
			value => Err(value_conversion_error(&value, "AtomOccurrenceRef")),
		}
	}
}

impl TryFrom<Value> for Path {
	type Error = FerrosGrynnError;

	fn try_from(value: Value) -> Result<Self, Self::Error> {
		match value {
			Value::Path(value) => Ok(*value),
			value => Err(value_conversion_error(&value, "Path")),
		}
	}
}

impl TryFrom<Value> for MaterializedGraph {
	type Error = FerrosGrynnError;

	fn try_from(value: Value) -> Result<Self, Self::Error> {
		match value {
			Value::Graph(value) => Ok(*value),
			value => Err(value_conversion_error(&value, "MaterializedGraph")),
		}
	}
}

impl TryFrom<Value> for Graph<Frozen> {
	type Error = FerrosGrynnError;

	fn try_from(value: Value) -> Result<Self, Self::Error> {
		match MaterializedGraph::try_from(value)? {
			MaterializedGraph::Graph(value) => Ok(value),
			value => Err(FerrosGrynnError::conversion_error(value.kind().name(), "Graph<Frozen>")),
		}
	}
}

macro_rules! try_graph_family_from_value {
	($($type:ty),* $(,)?) => {
		$(
			impl TryFrom<Value> for $type {
				type Error = FerrosGrynnError;

				fn try_from(value: Value) -> Result<Self, Self::Error> {
					Self::try_from(MaterializedGraph::try_from(value)?)
				}
			}
		)*
	};
}

try_graph_family_from_value!(
	graph::Tree<Frozen>,
	graph::RootedTree<Frozen>,
	graph::Forest<Frozen>,
	graph::Dag<Frozen>,
	graph::Hypergraph<Frozen>,
	graph::Metagraph<Frozen>,
	graph::Subgraph<Frozen>,
);

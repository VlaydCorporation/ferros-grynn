//! Exhaustive dispatch preserves each consumer's existing constant table.
/// Selects one pure caller value using the owner's current enum variants.
pub trait EnumChoice {
	/// Values in declaration order, checked without indexing.
	type Choices<T>;
	/// Chooses one already evaluated value.
	fn choose<T>(&self, choices: Self::Choices<T>) -> T;
}
impl<E: EnumChoice + ?Sized> EnumChoice for &E {
	type Choices<T> = E::Choices<T>;

	fn choose<T>(&self, choices: Self::Choices<T>) -> T {
		E::choose(*self, choices)
	}
}

impl EnumChoice for crate::graph::id::AtomKind {
	type Choices<T> = [T; 2];

	fn choose<T>(&self, choices: Self::Choices<T>) -> T {
		let [variant_0, variant_1] = choices;
		match self {
			Self::Node => variant_0,
			Self::Edge => variant_1,
		}
	}
}

impl EnumChoice for crate::graph::topology::EdgeDirection {
	type Choices<T> = [T; 3];

	fn choose<T>(&self, choices: Self::Choices<T>) -> T {
		let [variant_0, variant_1, variant_2] = choices;
		match self {
			Self::Directed => variant_0,
			Self::Bidirectional => variant_1,
			Self::Undirected => variant_2,
		}
	}
}

impl EnumChoice for crate::graph::path::EndpointRole {
	type Choices<T> = [T; 3];

	fn choose<T>(&self, choices: Self::Choices<T>) -> T {
		let [variant_0, variant_1, variant_2] = choices;
		match self {
			Self::Source => variant_0,
			Self::Target => variant_1,
			Self::Undirected => variant_2,
		}
	}
}

impl EnumChoice for crate::values::runtime::geometry::GeometryKind {
	type Choices<T> = [T; 7];

	fn choose<T>(&self, choices: Self::Choices<T>) -> T {
		let [variant_0, variant_1, variant_2, variant_3, variant_4, variant_5, variant_6] = choices;
		match self {
			Self::Point => variant_0,
			Self::MultiPoint => variant_1,
			Self::LineString => variant_2,
			Self::MultiLineString => variant_3,
			Self::Polygon => variant_4,
			Self::MultiPolygon => variant_5,
			Self::Collection => variant_6,
		}
	}
}

impl EnumChoice for crate::graph::read::GraphKind {
	type Choices<T> = [T; 8];

	fn choose<T>(&self, choices: Self::Choices<T>) -> T {
		let [
			variant_0,
			variant_1,
			variant_2,
			variant_3,
			variant_4,
			variant_5,
			variant_6,
			variant_7,
		] = choices;
		match self {
			Self::Graph => variant_0,
			Self::Tree => variant_1,
			Self::RootedTree => variant_2,
			Self::Forest => variant_3,
			Self::Dag => variant_4,
			Self::Hypergraph => variant_5,
			Self::Metagraph => variant_6,
			Self::Subgraph => variant_7,
		}
	}
}

impl EnumChoice for crate::graph::read::GraphStorageMode {
	type Choices<T> = [T; 3];

	fn choose<T>(&self, choices: Self::Choices<T>) -> T {
		let [variant_0, variant_1, variant_2] = choices;
		match self {
			Self::InMemory => variant_0,
			Self::DiskBacked => variant_1,
			Self::Hybrid => variant_2,
		}
	}
}

impl EnumChoice for crate::graph::read::GraphTopologyConstraint {
	type Choices<T> = [T; 3];

	fn choose<T>(&self, choices: Self::Choices<T>) -> T {
		let [variant_0, variant_1, variant_2] = choices;
		match self {
			Self::Simple => variant_0,
			Self::Bipartite => variant_1,
			Self::Complete => variant_2,
		}
	}
}

impl EnumChoice for crate::graph::path::PathDirection {
	type Choices<T> = [T; 3];

	fn choose<T>(&self, choices: Self::Choices<T>) -> T {
		let [variant_0, variant_1, variant_2] = choices;
		match self {
			Self::Forward => variant_0,
			Self::Reverse => variant_1,
			Self::Undirected => variant_2,
		}
	}
}

impl EnumChoice for crate::graph::traversal::index::TraversalDirection {
	type Choices<T> = [T; 3];

	fn choose<T>(&self, choices: Self::Choices<T>) -> T {
		let [variant_0, variant_1, variant_2] = choices;
		match self {
			Self::Forward => variant_0,
			Self::Backward => variant_1,
			Self::Both => variant_2,
		}
	}
}

impl EnumChoice for crate::values::runtime::vector::VectorKind {
	type Choices<T> = [T; 8];

	fn choose<T>(&self, choices: Self::Choices<T>) -> T {
		let [
			variant_0,
			variant_1,
			variant_2,
			variant_3,
			variant_4,
			variant_5,
			variant_6,
			variant_7,
		] = choices;
		match self {
			Self::F64 => variant_0,
			Self::F32 => variant_1,
			Self::F16 => variant_2,
			Self::I64 => variant_3,
			Self::I32 => variant_4,
			Self::I16 => variant_5,
			Self::I8 => variant_6,
			Self::U8 => variant_7,
		}
	}
}

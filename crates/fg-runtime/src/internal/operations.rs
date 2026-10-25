//! Pure value and incidence operations shared with engine consumers.
use crate::{
	graph::{AtomId, EdgeEndpoint, tensor::TensorAtom},
	values::{Number, Value, Vector},
};

/// Preserves the existing monotone_path_prefix operation in the value owner.
pub fn monotone_path_prefix(cost: Number) -> bool {
	match cost {
		Number::Float(_) => true,
		Number::Int(value) => Number::Float(value as f64) >= cost,
		Number::Decimal(_) => false,
	}
}

/// Preserves the existing atom_value operation in the value owner.
pub fn tensor_atom_into_value(atom: TensorAtom) -> Value {
	match atom {
		TensorAtom::Occurrence(atom) => Value::AtomOccurrence(Box::new(atom)),
		TensorAtom::Runtime {
			atom,
			..
		} => Value::Atom(Box::new(atom)),
	}
}

/// Preserves the existing component operation in the value owner.
///
/// # Panics
/// Panics if `index` is outside the vector, as in the original ANN kernel.
pub fn vector_component_as_f64(vector: &Vector, index: usize) -> f64 {
	match vector {
		Vector::F64(values) => values[index],
		Vector::F32(values) => f64::from(values[index]),
		Vector::F16(values) => values[index].to_f64(),
		Vector::I64(values) => values[index] as f64,
		Vector::I32(values) => f64::from(values[index]),
		Vector::I16(values) => f64::from(values[index]),
		Vector::I8(values) => f64::from(values[index]),
		Vector::U8(values) => f64::from(values[index]),
	}
}

/// Returns the existing encoded element width without changing caller
/// arithmetic.
pub fn vector_element_width(vector: &Vector) -> usize {
	match vector {
		Vector::F64(_) | Vector::I64(_) => 8,
		Vector::F32(_) | Vector::I32(_) => 4,
		Vector::F16(_) | Vector::I16(_) => 2,
		Vector::I8(_) | Vector::U8(_) => 1,
	}
}

/// Borrows an endpoint route and returns its terminal without copying.
pub fn endpoint_parts(endpoint: &EdgeEndpoint) -> (&[AtomId], AtomId) {
	match endpoint {
		EdgeEndpoint::Local(atom) => (&[], *atom),
		EdgeEndpoint::Nested(qualified) => (qualified.scope(), qualified.atom()),
	}
}
/// Returns the local outermost atom needed to materialize an endpoint route.
pub fn endpoint_outermost(endpoint: &EdgeEndpoint) -> Option<AtomId> {
	match endpoint {
		EdgeEndpoint::Local(atom) => Some(*atom),
		EdgeEndpoint::Nested(qualified) => qualified.scope().first().copied(),
	}
}

/// Visits the original endpoint side groups used by adjacency accounting.
#[allow(
	clippy::impl_trait_in_params,
	reason = "Exact-version internal call signatures remain unchanged during extraction"
)]
pub fn visit_adjacency_endpoints(
	topology: &crate::graph::EdgeTopology,
	mut append: impl FnMut(&EdgeEndpoint, &[usize]),
) {
	match topology {
		crate::graph::EdgeTopology::Directed {
			source,
			target,
		} => {
			for endpoint in source {
				append(endpoint, &[0]);
			}
			for endpoint in target {
				append(endpoint, &[1]);
			}
		}
		crate::graph::EdgeTopology::Bidirectional {
			source,
			target,
		} => {
			for endpoint in source.iter().chain(target) {
				append(endpoint, &[0, 1]);
			}
		}
		crate::graph::EdgeTopology::Undirected {
			endpoints,
		} => {
			for endpoint in endpoints {
				append(endpoint, &[2]);
			}
		}
	}
}

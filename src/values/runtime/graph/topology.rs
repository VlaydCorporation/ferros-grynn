use std::fmt::{self, Display, Formatter};

use smallvec::SmallVec;

use super::AtomId;
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

const INLINE_ENDPOINTS: usize = 4;

/// An atom inside a chain of nested metagraphs.
#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub struct QualifiedAtomId {
	scope: Box<[AtomId]>,
	atom: AtomId,
}

impl QualifiedAtomId {
	pub fn new<IntoVec: Into<Vec<AtomId>>>(scope: IntoVec, atom: AtomId) -> FerrosGrynnResult<Self> {
		let scope = scope.into();
		if scope.is_empty() {
			return Err(FerrosGrynnError::graph_constraint_violation(
				"metagraph",
				"a qualified atom must contain at least one metavertex in its scope",
			));
		}
		Ok(Self {
			scope: scope.into_boxed_slice(),
			atom,
		})
	}

	pub fn scope(&self) -> &[AtomId] {
		&self.scope
	}

	pub const fn atom(&self) -> AtomId {
		self.atom
	}
}

/// A local or cross-level endpoint of an edge.
#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum EdgeEndpoint {
	Local(AtomId),
	Nested(QualifiedAtomId),
}

impl EdgeEndpoint {
	pub const fn local_id(&self) -> Option<AtomId> {
		match self {
			Self::Local(id) => Some(*id),
			Self::Nested(_) => None,
		}
	}

	pub const fn is_cross_level(&self) -> bool {
		matches!(self, Self::Nested(_))
	}
}

impl From<AtomId> for EdgeEndpoint {
	fn from(value: AtomId) -> Self {
		Self::Local(value)
	}
}

impl From<QualifiedAtomId> for EdgeEndpoint {
	fn from(value: QualifiedAtomId) -> Self {
		Self::Nested(value)
	}
}

#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum EdgeDirection {
	Directed,
	Bidirectional,
	Undirected,
}

impl Display for EdgeDirection {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter.write_str(match self {
			Self::Directed => "directed",
			Self::Bidirectional => "bidirectional",
			Self::Undirected => "undirected",
		})
	}
}

/// Unified source/target endpoint incidence.
#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum EdgeTopology {
	Directed {
		source: SmallVec<EdgeEndpoint, INLINE_ENDPOINTS>,
		target: SmallVec<EdgeEndpoint, INLINE_ENDPOINTS>,
	},
	Bidirectional {
		source: SmallVec<EdgeEndpoint, INLINE_ENDPOINTS>,
		target: SmallVec<EdgeEndpoint, INLINE_ENDPOINTS>,
	},
	Undirected {
		endpoints: SmallVec<EdgeEndpoint, INLINE_ENDPOINTS>,
	},
}

impl EdgeTopology {
	pub fn directed<S, T, SE, TE>(source: S, target: T) -> Self
	where
		S: IntoIterator<Item = SE>,
		T: IntoIterator<Item = TE>,
		SE: Into<EdgeEndpoint>,
		TE: Into<EdgeEndpoint>,
	{
		Self::Directed {
			source: source.into_iter().map(Into::into).collect(),
			target: target.into_iter().map(Into::into).collect(),
		}
	}

	pub fn bidirectional<S, T, SE, TE>(source: S, target: T) -> Self
	where
		S: IntoIterator<Item = SE>,
		T: IntoIterator<Item = TE>,
		SE: Into<EdgeEndpoint>,
		TE: Into<EdgeEndpoint>,
	{
		Self::Bidirectional {
			source: source.into_iter().map(Into::into).collect(),
			target: target.into_iter().map(Into::into).collect(),
		}
	}

	pub fn undirected<E, I>(endpoints: I) -> Self
	where
		I: IntoIterator<Item = E>,
		E: Into<EdgeEndpoint>,
	{
		Self::Undirected {
			endpoints: endpoints.into_iter().map(Into::into).collect(),
		}
	}

	pub const fn direction(&self) -> EdgeDirection {
		match self {
			Self::Directed {
				..
			} => EdgeDirection::Directed,
			Self::Bidirectional {
				..
			} => EdgeDirection::Bidirectional,
			Self::Undirected {
				..
			} => EdgeDirection::Undirected,
		}
	}

	pub fn source(&self) -> &[EdgeEndpoint] {
		match self {
			Self::Directed {
				source,
				..
			}
			| Self::Bidirectional {
				source,
				..
			} => source,
			Self::Undirected {
				endpoints,
			} => endpoints,
		}
	}

	pub fn target(&self) -> &[EdgeEndpoint] {
		match self {
			Self::Directed {
				target,
				..
			}
			| Self::Bidirectional {
				target,
				..
			} => target,
			Self::Undirected {
				..
			} => &[],
		}
	}

	pub fn endpoints(&self) -> impl Iterator<Item = &EdgeEndpoint> {
		self.source().iter().chain(self.target())
	}

	pub fn endpoint_count(&self) -> usize {
		self.source().len() + self.target().len()
	}

	pub fn is_hyper(&self) -> bool {
		self.endpoint_count() > 2 || self.source().len() > 1 || self.target().len() > 1
	}

	pub fn is_cross_level(&self) -> bool {
		self.endpoints().any(EdgeEndpoint::is_cross_level)
	}

	pub fn local_binary(&self) -> Option<(AtomId, AtomId, EdgeDirection)> {
		match self {
			Self::Directed {
				source,
				target,
			}
			| Self::Bidirectional {
				source,
				target,
			} if source.len() == 1 && target.len() == 1 => {
				Some((source[0].local_id()?, target[0].local_id()?, self.direction()))
			}
			Self::Undirected {
				endpoints,
			} if endpoints.len() == 2 => Some((
				endpoints[0].local_id()?,
				endpoints[1].local_id()?,
				EdgeDirection::Undirected,
			)),
			_ => None,
		}
	}

	pub(crate) fn try_map_endpoints(
		&self,
		mut map: impl FnMut(&EdgeEndpoint) -> FerrosGrynnResult<EdgeEndpoint>,
	) -> FerrosGrynnResult<Self> {
		Ok(match self {
			Self::Directed {
				source,
				target,
			} => Self::Directed {
				source: source.iter().map(&mut map).collect::<FerrosGrynnResult<_>>()?,
				target: target.iter().map(&mut map).collect::<FerrosGrynnResult<_>>()?,
			},
			Self::Bidirectional {
				source,
				target,
			} => Self::Bidirectional {
				source: source.iter().map(&mut map).collect::<FerrosGrynnResult<_>>()?,
				target: target.iter().map(&mut map).collect::<FerrosGrynnResult<_>>()?,
			},
			Self::Undirected {
				endpoints,
			} => Self::Undirected {
				endpoints: endpoints.iter().map(map).collect::<FerrosGrynnResult<_>>()?,
			},
		})
	}
}

#[cfg(test)]
mod tests {
	use super::{EdgeDirection, EdgeEndpoint, EdgeTopology, QualifiedAtomId};
	use crate::values::runtime::graph::{AtomId, AtomKind};

	#[test]
	fn distinguishes_binary_hyper_and_cross_level_topology() {
		let left = AtomId::from_parts(AtomKind::Node, 0, 0).unwrap();
		let right = AtomId::from_parts(AtomKind::Node, 1, 0).unwrap();
		let meta = AtomId::from_parts(AtomKind::Node, 2, 0).unwrap();
		let nested = QualifiedAtomId::new(vec![meta], left).unwrap();

		let binary = EdgeTopology::directed([left], [right]);
		assert_eq!(binary.local_binary(), Some((left, right, EdgeDirection::Directed)));
		assert!(!binary.is_hyper());

		let hyper =
			EdgeTopology::directed([EdgeEndpoint::Local(left), EdgeEndpoint::Nested(nested)], [
				EdgeEndpoint::Local(right),
			]);
		assert!(hyper.is_hyper());
		assert!(hyper.is_cross_level());
	}
}

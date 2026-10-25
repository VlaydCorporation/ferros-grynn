use std::fmt::{self, Display, Formatter};

use smallvec::SmallVec;

use super::AtomId;
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Configured inline endpoints value.
const INLINE_ENDPOINTS: usize = 4;

/// An atom inside a chain of nested metagraphs.
#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub struct QualifiedAtomId {
	/// Scope associated with this qualified atom id.
	scope: Box<[AtomId]>,
	/// Atom associated with this qualified atom id.
	atom: AtomId,
}

impl QualifiedAtomId {
	/// Constructs `QualifiedAtomId` from the supplied initial state.
	///
	/// # Errors
	///
	/// Returns `graph_constraint_violation` diagnostics.
	pub fn new<IntoVec: Into<Vec<AtomId>>>(
		scope: IntoVec,
		atom: AtomId,
	) -> FerrosGrynnResult<Self> {
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

	/// Borrows the stored scope of this `QualifiedAtomId`.
	pub fn scope(&self) -> &[AtomId] {
		&self.scope
	}

	/// Returns the stored atom of this `QualifiedAtomId`.
	pub const fn atom(&self) -> AtomId {
		self.atom
	}
}

/// A local or cross-level endpoint of an edge.
#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum EdgeEndpoint {
	/// Endpoint addresses an atom in the current graph materialization.
	Local(
		/// Endpoint addresses an atom in the current graph materialization.
		AtomId,
	),
	/// Endpoint addresses an atom across a nested graph boundary.
	Nested(
		/// Endpoint addresses an atom across a nested graph boundary.
		QualifiedAtomId,
	),
}

impl EdgeEndpoint {
	/// Returns the kind of the endpoint while preserving its qualified scope.
	pub(crate) const fn kind(&self) -> Option<crate::graph::AtomKind> {
		match self {
			Self::Local(atom) => atom.kind(),
			Self::Nested(qualified) => qualified.atom().kind(),
		}
	}

	/// Returns local id for `EdgeEndpoint`.
	pub const fn local_id(&self) -> Option<AtomId> {
		match self {
			Self::Local(id) => Some(*id),
			Self::Nested(_) => None,
		}
	}

	/// Reports whether the value is cross level for `EdgeEndpoint`.
	pub const fn is_cross_level(&self) -> bool {
		matches!(self, Self::Nested(_))
	}
}

impl From<AtomId> for EdgeEndpoint {
	/// Converts `AtomId` into `EdgeEndpoint`.
	fn from(value: AtomId) -> Self {
		Self::Local(value)
	}
}

impl From<QualifiedAtomId> for EdgeEndpoint {
	/// Converts `QualifiedAtomId` into `EdgeEndpoint`.
	fn from(value: QualifiedAtomId) -> Self {
		Self::Nested(value)
	}
}

/// Directed, bidirectional or undirected interpretation of an edge topology.
#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum EdgeDirection {
	/// Incidence distinguishes source and target sides.
	Directed,
	/// Incidence permits traversal in both directed orientations.
	Bidirectional,
	/// Incidence has no source-or-target orientation.
	Undirected,
}

impl Display for EdgeDirection {
	/// Formats `EdgeDirection` using the requested formatter.
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
	/// Distinct source and target endpoint collections.
	Directed {
		/// Source-side endpoints of the directed incidence.
		source: SmallVec<EdgeEndpoint, INLINE_ENDPOINTS>,
		/// Target-side endpoints of the directed incidence.
		target: SmallVec<EdgeEndpoint, INLINE_ENDPOINTS>,
	},
	/// Endpoint collections traversable in both directed orientations.
	Bidirectional {
		/// Source-side endpoints of the directed incidence.
		source: SmallVec<EdgeEndpoint, INLINE_ENDPOINTS>,
		/// Target-side endpoints of the directed incidence.
		target: SmallVec<EdgeEndpoint, INLINE_ENDPOINTS>,
	},
	/// Unoriented endpoint collection.
	Undirected {
		/// Endpoint collection defining the edge or hyperedge incidence.
		endpoints: SmallVec<EdgeEndpoint, INLINE_ENDPOINTS>,
	},
}

impl EdgeTopology {
	/// Retained endpoint slots, including spare heap capacity of mutable
	/// topologies.
	pub(crate) fn retained_endpoint_slots(&self) -> usize {
		match self {
			Self::Directed {
				source,
				target,
			}
			| Self::Bidirectional {
				source,
				target,
			} => {
				let source_slots = if source.capacity() > INLINE_ENDPOINTS {
					source.capacity()
				} else {
					source.len()
				};
				let target_slots = if target.capacity() > INLINE_ENDPOINTS {
					target.capacity()
				} else {
					target.len()
				};
				source_slots.saturating_add(target_slots)
			}
			Self::Undirected {
				endpoints,
			} => {
				if endpoints.capacity() > INLINE_ENDPOINTS {
					endpoints.capacity()
				} else {
					endpoints.len()
				}
			}
		}
	}

	/// Returns directed for `EdgeTopology`.
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

	/// Returns bidirectional for `EdgeTopology`.
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

	/// Returns undirected for `EdgeTopology`.
	pub fn undirected<E, I>(endpoints: I) -> Self
	where
		I: IntoIterator<Item = E>,
		E: Into<EdgeEndpoint>,
	{
		Self::Undirected {
			endpoints: endpoints.into_iter().map(Into::into).collect(),
		}
	}

	/// Returns direction for `EdgeTopology`.
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

	/// Returns source for `EdgeTopology`.
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

	/// Returns target for `EdgeTopology`.
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

	/// Returns endpoints for `EdgeTopology`.
	pub fn endpoints(&self) -> impl Iterator<Item = &EdgeEndpoint> {
		self.source().iter().chain(self.target())
	}

	/// Enumerates distinct-side edge endpoint keys for synchronous postings.
	pub(crate) fn edge_endpoint_sides(&self) -> impl Iterator<Item = (&EdgeEndpoint, bool)> {
		self.source()
			.iter()
			.map(|endpoint| (endpoint, false))
			.chain(self.target().iter().map(|endpoint| (endpoint, true)))
			.filter(|(endpoint, _)| endpoint.kind() == Some(crate::graph::AtomKind::Edge))
	}

	/// Returns endpoint count for `EdgeTopology`.
	pub fn endpoint_count(&self) -> usize {
		self.source().len() + self.target().len()
	}

	/// Reports whether the value is hyper for `EdgeTopology`.
	pub fn is_hyper(&self) -> bool {
		self.endpoint_count() > 2 || self.source().len() > 1 || self.target().len() > 1
	}

	/// Reports whether the value is cross level for `EdgeTopology`.
	pub fn is_cross_level(&self) -> bool {
		self.endpoints().any(EdgeEndpoint::is_cross_level)
	}

	/// Returns local binary for `EdgeTopology`.
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

	/// Attempts map endpoints.
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
/// Regression tests for topology behavior.
mod tests {
	use super::{EdgeDirection, EdgeEndpoint, EdgeTopology, QualifiedAtomId};
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;
	use crate::values::runtime::graph::{AtomId, AtomKind};

	/// Verifies distinguishes binary hyper and cross level topology.
	#[test]
	fn distinguishes_binary_hyper_and_cross_level_topology() {
		let left = AtomId::from_parts(AtomKind::Node, 0, 0).assert_ok();
		let right = AtomId::from_parts(AtomKind::Node, 1, 0).assert_ok();
		let meta = AtomId::from_parts(AtomKind::Node, 2, 0).assert_ok();
		let nested = QualifiedAtomId::new(vec![meta], left).assert_ok();

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

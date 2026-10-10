//! Canonical owned identities of LIVE results.

use crate::values::{AtomOccurrenceRef, RecordId, Value};

/// Logical result identity; graph occurrences retain their qualified scope.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum LiveIdentity {
	/// One database-global source record.
	Record(RecordId),
	/// One graph-qualified occurrence, including repeated base atoms in scopes.
	Occurrence(AtomOccurrenceRef),
	/// One aggregate group, identified by its exact ordered group keys.
	/// Empty keys denote an aggregate over the complete selected source.
	Aggregate(Vec<Value>),
	/// Expanded row participants in FROM/JOIN order, followed by zero-based
	/// SPLIT positions in clause order. Unmatched LEFT participants are
	/// explicit. The primary source is required and at least two parts must
	/// be present. GROUP results retain their Aggregate identity.
	///
	/// ```
	/// # fn main() -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
	/// use ferros_grynn::live::{LiveIdentity, LiveIdentityPart};
	/// let row = LiveIdentity::Composite(vec![
	///     LiveIdentityPart::Record("person:alice".parse()?),
	///     LiveIdentityPart::Unmatched,
	///     LiveIdentityPart::Split(u64::MAX),
	/// ]);
	/// assert!(matches!(row, LiveIdentity::Composite(parts) if parts.len() == 3));
	/// # Ok(())
	/// # }
	/// ```
	Composite(Vec<LiveIdentityPart>),
}

impl LiveIdentity {
	/// Logical identity heap accounting never consults physical records.
	pub(crate) fn estimated_heap_bytes(&self) -> usize {
		match self {
			Self::Record(record) => record.estimated_heap_bytes(),
			Self::Occurrence(occurrence) => occurrence
				.atom()
				.id()
				.estimated_heap_bytes()
				.saturating_add(occurrence.graph().name().len())
				.saturating_add(128),
			Self::Aggregate(keys) => keys.iter().fold(
				keys.capacity().saturating_mul(std::mem::size_of::<Value>()),
				|bytes, value| bytes.saturating_add(value.estimated_heap_bytes()),
			),
			Self::Composite(parts) => parts.iter().fold(
				parts
					.capacity()
					.saturating_mul(std::mem::size_of::<crate::live::LiveIdentityPart>()),
				|bytes, part| {
					bytes.saturating_add(match part {
						LiveIdentityPart::Record(record) => record.estimated_heap_bytes(),
						LiveIdentityPart::Occurrence(occurrence) => occurrence
							.atom()
							.id()
							.estimated_heap_bytes()
							.saturating_add(occurrence.graph().name().len())
							.saturating_add(128),
						LiveIdentityPart::Unmatched | LiveIdentityPart::Split(_) => 0,
					})
				},
			),
		}
	}

	/// Canonical kind order, independent of Rust enum representation.
	const fn rank(&self) -> u8 {
		match self {
			Self::Record(_) => 0,
			Self::Occurrence(_) => 1,
			Self::Aggregate(_) => 2,
			Self::Composite(_) => 3,
		}
	}
}

impl PartialEq for LiveIdentity {
	fn eq(&self, other: &Self) -> bool {
		self.cmp(other).is_eq()
	}
}
impl Eq for LiveIdentity {}
impl PartialOrd for LiveIdentity {
	fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
		Some(self.cmp(other))
	}
}
impl Ord for LiveIdentity {
	fn cmp(&self, other: &Self) -> std::cmp::Ordering {
		match (self, other) {
			(Self::Record(left), Self::Record(right)) => left.cmp(right),
			(Self::Occurrence(left), Self::Occurrence(right)) => {
				(left.graph().id(), left.atom()).cmp(&(right.graph().id(), right.atom()))
			}
			(Self::Aggregate(left), Self::Aggregate(right)) => left.cmp(right),
			(Self::Composite(left), Self::Composite(right)) => left.cmp(right),
			_ => self.rank().cmp(&other.rank()),
		}
	}
}
impl std::hash::Hash for LiveIdentity {
	fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
		match self {
			Self::Record(record) => {
				0_u8.hash(state);
				record.hash(state);
			}
			Self::Occurrence(occurrence) => {
				1_u8.hash(state);
				occurrence.graph().id().hash(state);
				occurrence.atom().hash(state);
			}
			Self::Aggregate(keys) => {
				2_u8.hash(state);
				keys.hash(state);
			}
			Self::Composite(parts) => {
				3_u8.hash(state);
				parts.hash(state);
			}
		}
	}
}

/// One nonrecursive component of an expanded LIVE result identity.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum LiveIdentityPart {
	/// One database-global participant at its declaration position.
	Record(RecordId),
	/// One qualified participant, independent of its graph's display name.
	Occurrence(AtomOccurrenceRef),
	/// No right participant in a LEFT JOIN; distinct from a present record
	/// whose projected fields are NONE or NULL.
	Unmatched,
	/// Zero-based position in one SPLIT clause, retaining array duplicates.
	Split(u64),
}

impl LiveIdentityPart {
	/// Canonical part order, independent of Rust enum representation.
	const fn rank(&self) -> u8 {
		match self {
			Self::Record(_) => 0,
			Self::Occurrence(_) => 1,
			Self::Unmatched => 2,
			Self::Split(_) => 3,
		}
	}
}
impl PartialEq for LiveIdentityPart {
	fn eq(&self, other: &Self) -> bool {
		self.cmp(other).is_eq()
	}
}
impl Eq for LiveIdentityPart {}
impl PartialOrd for LiveIdentityPart {
	fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
		Some(self.cmp(other))
	}
}
impl Ord for LiveIdentityPart {
	fn cmp(&self, other: &Self) -> std::cmp::Ordering {
		match (self, other) {
			(Self::Record(left), Self::Record(right)) => left.cmp(right),
			(Self::Occurrence(left), Self::Occurrence(right)) => {
				(left.graph().id(), left.atom()).cmp(&(right.graph().id(), right.atom()))
			}
			(Self::Unmatched, Self::Unmatched) => std::cmp::Ordering::Equal,
			(Self::Split(left), Self::Split(right)) => left.cmp(right),
			_ => self.rank().cmp(&other.rank()),
		}
	}
}
impl std::hash::Hash for LiveIdentityPart {
	fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
		self.rank().hash(state);
		match self {
			Self::Record(record) => record.hash(state),
			Self::Occurrence(occurrence) => {
				occurrence.graph().id().hash(state);
				occurrence.atom().hash(state);
			}
			Self::Unmatched => {}
			Self::Split(position) => position.hash(state),
		}
	}
}

/// Shape authority shared by trusted producers and checked physical codecs.
/// Caller admission must cover the flat traversal before calling this helper.
pub fn valid_composite_parts(parts: &[LiveIdentityPart]) -> bool {
	if parts.len() < 2
		|| !matches!(
			parts.first(),
			Some(LiveIdentityPart::Record(_) | LiveIdentityPart::Occurrence(_))
		) {
		return false;
	}
	let mut positions = false;
	for part in parts {
		match part {
			LiveIdentityPart::Split(_) => positions = true,
			_ if positions => return false,
			_ => {}
		}
	}
	true
}

#[cfg(test)]
mod tests {
	use std::{
		collections::BTreeSet,
		hash::{DefaultHasher, Hash, Hasher},
		sync::Arc,
	};

	use super::*;
	use crate::{
		gql_status::{FerrosGrynnError, FerrosGrynnResult},
		graph::{AtomKind, AtomRef, GraphId, GraphKind, GraphRef, GraphStorageMode},
	};

	/// Scope, logical slot order and exact positions distinguish bag copies;
	/// graph presentation changes preserve equality and hashing.
	#[test]
	fn live_composite_identity_order_hash_and_shape() -> FerrosGrynnResult<()> {
		let record: RecordId = "item:a".parse()?;
		let qualified = |id, name| -> FerrosGrynnResult<_> {
			Ok(LiveIdentityPart::Occurrence(AtomOccurrenceRef::new(
				AtomRef::new(AtomKind::Node, record.clone()),
				Arc::new(GraphRef::new(
					GraphId::new(id)
						.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture graph"))?,
					name,
					GraphKind::Graph,
					GraphStorageMode::InMemory,
				)?),
			)))
		};
		let first = vec![
			qualified(1, "before")?,
			LiveIdentityPart::Unmatched,
			LiveIdentityPart::Split(u64::MAX),
		];
		let renamed = vec![
			qualified(1, "after")?,
			LiveIdentityPart::Unmatched,
			LiveIdentityPart::Split(u64::MAX),
		];
		let other = vec![
			qualified(2, "before")?,
			LiveIdentityPart::Unmatched,
			LiveIdentityPart::Split(u64::MAX),
		];
		assert!(valid_composite_parts(&first));
		let hash = |identity: &LiveIdentity| {
			let mut state = DefaultHasher::new();
			identity.hash(&mut state);
			state.finish()
		};
		let first = LiveIdentity::Composite(first);
		let renamed = LiveIdentity::Composite(renamed);
		assert_eq!(first, renamed);
		assert_eq!(hash(&first), hash(&renamed));
		assert_ne!(first, LiveIdentity::Composite(other));
		let duplicates = [0, 1, (1_u64 << 53) + 1, u64::MAX].map(|position| {
			LiveIdentity::Composite(vec![
				LiveIdentityPart::Record(record.clone()),
				LiveIdentityPart::Split(position),
			])
		});
		assert!(duplicates.windows(2).all(|pair| pair[0] < pair[1]));
		assert_eq!(duplicates.into_iter().collect::<BTreeSet<_>>().len(), 4);
		let ordered =
			[LiveIdentity::Record(record.clone()), LiveIdentity::Aggregate(Vec::new()), first];
		assert!(ordered.windows(2).all(|pair| pair[0] < pair[1]));
		for parts in [
			vec![],
			vec![LiveIdentityPart::Record(record.clone())],
			vec![LiveIdentityPart::Unmatched, LiveIdentityPart::Split(0)],
			vec![
				LiveIdentityPart::Record(record.clone()),
				LiveIdentityPart::Split(0),
				LiveIdentityPart::Unmatched,
			],
			vec![
				LiveIdentityPart::Record(record.clone()),
				LiveIdentityPart::Split(0),
				LiveIdentityPart::Record(record.clone()),
			],
		] {
			assert!(!valid_composite_parts(&parts));
		}
		assert!(valid_composite_parts(&[
			LiveIdentityPart::Record(record.clone()),
			LiveIdentityPart::Record(record)
		]));
		Ok(())
	}
}

//! Snapshot-owned authoritative mappings and independent bidirectional indexes.

use std::{
	collections::{BTreeMap, BTreeSet},
	hash::{Hash, Hasher},
	sync::Arc,
};

use crate::{
	bridge::{
		BridgeDatabase,
		BridgeDatabaseId,
		BridgeEntityRef,
		BridgeMapping,
		BridgeMappingId,
		BridgeMappingRole,
		BridgeMappingState,
		BridgeMappingSubject,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{RecordId, Text},
};

/// COW buckets bound mutation copies without duplicating authoritative rows.
#[derive(Clone)]
struct RegistryMap<K, V> {
	/// Shared COW shard table; an empty registry allocates none.
	buckets: Option<Arc<[Arc<RegistryBucket<K, V>>]>>,
}

/// Owned-key accounting avoids scanning a growing bucket during admission.
#[derive(Clone)]
struct RegistryBucket<K, V> {
	/// Ordered entries owned by this individual COW shard.
	entries: BTreeMap<K, V>,
	/// Cached heap cost of owned keys in this shard.
	owned_bytes: usize,
}

impl<K, V> Default for RegistryMap<K, V> {
	fn default() -> Self {
		Self {
			buckets: None,
		}
	}
}

impl<K: Ord + Clone + Hash, V: Clone> RegistryMap<K, V> {
	/// Selects the deterministic COW shard for an indexed key.
	fn bucket(key: &K) -> usize {
		let mut hash = rapidhash::fast::RapidHasher::default();
		key.hash(&mut hash);
		(hash.finish() & 255) as usize
	}

	/// Borrows the indexed authoritative entry without scanning other shards.
	fn get(&self, key: &K) -> Option<&V> {
		self.buckets.as_ref()?.get(Self::bucket(key))?.entries.get(key)
	}

	/// Copies only the affected shard and updates cached owned-key accounting.
	fn insert(&mut self, key: K, value: V, owned_bytes: usize) -> Option<V> {
		let buckets = self.buckets.get_or_insert_with(|| {
			(0..256)
				.map(|_| {
					Arc::new(RegistryBucket {
						entries: BTreeMap::new(),
						owned_bytes: 0,
					})
				})
				.collect()
		});
		let bucket = Arc::make_mut(&mut Arc::make_mut(buckets)[Self::bucket(&key)]);
		let previous = bucket.entries.insert(key, value);
		if previous.is_none() {
			bucket.owned_bytes = bucket.owned_bytes.saturating_add(owned_bytes);
		}
		previous
	}

	/// Removes the indexed entry and its cached owned-key charge.
	fn remove(&mut self, key: &K, owned_bytes: usize) -> Option<V> {
		let buckets = self.buckets.as_mut()?;
		let bucket = Arc::make_mut(&mut Arc::make_mut(buckets)[Self::bucket(key)]);
		let previous = bucket.entries.remove(key);
		if previous.is_some() {
			bucket.owned_bytes = bucket.owned_bytes.saturating_sub(owned_bytes);
		}
		previous
	}

	/// Visits authoritative entries across the allocated shards.
	fn values(&self) -> impl Iterator<Item = &V> {
		self.buckets
			.iter()
			.flat_map(|buckets| buckets.iter())
			.flat_map(|bucket| bucket.entries.values())
	}

	/// Checks the affected shard copy cost before mutation.
	fn copy_bytes(&self, key: &K, entry_bytes: usize) -> FerrosGrynnResult<usize> {
		let count =
			self.buckets.as_ref().map_or(0, |buckets| buckets[Self::bucket(key)].entries.len());
		let owned =
			self.buckets.as_ref().map_or(0, |buckets| buckets[Self::bucket(key)].owned_bytes);
		crate::values::storage::tree_allocation_bytes(count.saturating_add(1), entry_bytes)?
			.checked_add(256 * std::mem::size_of::<Arc<RegistryBucket<K, V>>>() + 128)
			.and_then(|bytes| bytes.checked_add(owned))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("bridge registry COW admission"))
	}
}

/// Local subject and external database uniqueness domain.
type DirectKey = (BridgeMappingSubject, BridgeDatabaseId);
/// External identity and mapping role uniqueness domain.
type ReverseKey = (BridgeEntityRef, BridgeMappingRole);

/// Empty registries allocate no bucket arrays; ordinary root clones copy one
/// Arc.
#[derive(Clone, Default)]
pub struct BridgeRegistry {
	/// Authoritative non-secret external database descriptors.
	pub(crate) databases: BTreeMap<BridgeDatabaseId, Arc<BridgeDatabase>>,
	/// Alias index returning descriptor identities.
	pub(crate) aliases: BTreeMap<Text, BridgeDatabaseId>,
	/// Authoritative mapping records shared through immutable Arcs.
	rows: RegistryMap<BridgeMappingId, Arc<BridgeMapping>>,
	/// Local subject and database index returning authoritative row identities.
	direct: RegistryMap<DirectKey, BridgeMappingId>,
	/// External identity and role index returning authoritative row identities.
	reverse: RegistryMap<ReverseKey, BridgeMappingId>,
	/// Committed logical generation, independent of physical locators.
	pub(crate) generation: u64,
	/// Last accepted immutable physical roots; rows remain logical authority.
	pub(crate) physical: Option<crate::storage::BridgeCatalogBinding>,
	/// Private replay tracks domains changed by cascading subject deletion.
	pub(crate) touched: BTreeSet<(BridgeDatabaseId, BridgeMappingRole)>,
}

impl BridgeRegistry {
	/// Authoritative mapping records shared through immutable Arcs.
	pub(crate) fn rows(&self) -> impl Iterator<Item = &Arc<BridgeMapping>> {
		self.rows.values()
	}

	/// Borrows the indexed authoritative entry without scanning other shards.
	pub(crate) fn get(&self, id: BridgeMappingId) -> Option<&Arc<BridgeMapping>> {
		self.rows.get(&id)
	}

	/// Looks up one local subject through the direct index.
	pub(crate) fn local(
		&self,
		database: BridgeDatabaseId,
		subject: &BridgeMappingSubject,
	) -> Option<&Arc<BridgeMapping>> {
		self.get(*self.direct.get(&(subject.clone(), database))?)
	}

	/// External database and native record identity.
	pub(crate) fn external(
		&self,
		entity: &BridgeEntityRef,
		role: BridgeMappingRole,
	) -> Option<&Arc<BridgeMapping>> {
		self.get(*self.reverse.get(&(entity.clone(), role))?)
	}

	/// Publishes a unique descriptor identity and runtime alias.
	pub(crate) fn install_database(
		&mut self,
		database: Arc<BridgeDatabase>,
	) -> FerrosGrynnResult<()> {
		let id = database.reference.id;
		if self.databases.contains_key(&id) || self.aliases.contains_key(&database.alias) {
			return Err(FerrosGrynnError::bridge_mapping_error(
				"bridge database identity or alias is already registered",
			));
		}
		self.aliases.insert(database.alias.clone(), id);
		self.databases.insert(id, database);
		self.advance()?;
		Ok(())
	}

	/// Bounds new authority, index keys and the recoverable intent before
	/// cloning.
	pub(crate) fn admit_row(&self, row: &BridgeMapping) -> FerrosGrynnResult<usize> {
		self.admit_copy(row, true)
	}

	/// Computes the affected COW shards and optional new payload charge.
	fn admit_copy(&self, row: &BridgeMapping, payload: bool) -> FerrosGrynnResult<usize> {
		let direct = (row.subject.clone(), row.external.database.id);
		let reverse = (row.external.clone(), row.subject.role());
		let mut bytes = self.rows.copy_bytes(
			&row.reference.id,
			std::mem::size_of::<(BridgeMappingId, Arc<BridgeMapping>)>(),
		)?;
		let direct_bytes =
			self.direct.copy_bytes(&direct, std::mem::size_of::<(DirectKey, BridgeMappingId)>())?;
		let reverse_bytes = self
			.reverse
			.copy_bytes(&reverse, std::mem::size_of::<(ReverseKey, BridgeMappingId)>())?;
		bytes = bytes
			.checked_add(direct_bytes)
			.and_then(|bytes| bytes.checked_add(reverse_bytes))
			.ok_or_else(|| {
				FerrosGrynnError::operation_overflow("bridge registry write admission")
			})?;
		let values = if payload {
			row.mirror.as_ref().map_or(Ok(0_usize), |intent| {
				intent.properties.iter().try_fold(
					intent
						.relation
						.estimated_heap_bytes()
						.saturating_add(intent.source.estimated_heap_bytes())
						.saturating_add(intent.target.estimated_heap_bytes()),
					|bytes, (name, value)| {
						bytes
							.checked_add(name.len())
							.and_then(|bytes| bytes.checked_add(value.estimated_heap_bytes()))
							.ok_or_else(|| {
								FerrosGrynnError::operation_overflow("bridge intent properties")
							})
					},
				)
			})?
		} else {
			0
		};
		let identities = row
			.subject
			.record()
			.estimated_heap_bytes()
			.checked_add(row.external.record.estimated_heap_bytes())
			.and_then(|bytes| bytes.checked_mul(2))
			.ok_or_else(|| {
				FerrosGrynnError::operation_overflow("bridge mapping identity copies")
			})?;
		bytes = bytes.checked_add(identities).ok_or_else(|| {
			FerrosGrynnError::operation_overflow("bridge mapping identity admission")
		})?;
		bytes.checked_add(values).and_then(|bytes| bytes.checked_add(4096)).ok_or_else(|| {
			FerrosGrynnError::operation_overflow("bridge registry payload admission")
		})
	}

	/// Bounds a lifecycle transition while retaining the shared immutable
	/// intent.
	pub(crate) fn admit_transition(&self, row: &BridgeMapping) -> FerrosGrynnResult<usize> {
		self.admit_copy(row, false)
	}

	/// Installs authority and both uniqueness indexes in the same private root.
	pub(crate) fn install(&mut self, row: Arc<BridgeMapping>) -> FerrosGrynnResult<()> {
		let direct = (row.subject.clone(), row.external.database.id);
		let reverse = (row.external.clone(), row.subject.role());
		let id = row.reference.id;
		let domain = (row.external.database.id, row.subject.role());
		if self.rows.get(&id).is_some() || !self.databases.contains_key(&row.external.database.id) {
			return Err(FerrosGrynnError::bridge_mapping_error(
				"mapping identity exists or database is absent",
			));
		}
		if row.state != BridgeMappingState::Tombstoned {
			if self.direct.get(&direct).is_some() || self.reverse.get(&reverse).is_some() {
				return Err(FerrosGrynnError::bridge_mapping_error(
					"mapping violates bidirectional uniqueness",
				));
			}
			self.direct.insert(direct, id, row.subject.record().estimated_heap_bytes());
			self.reverse.insert(reverse, id, row.external.record.estimated_heap_bytes());
		}
		self.rows.insert(id, row, 0);
		self.touched.insert(domain);
		self.advance()?;
		Ok(())
	}

	/// Updates one lifecycle state and releases indexes only for a tombstone.
	pub(crate) fn transition(
		&mut self,
		id: BridgeMappingId,
		state: BridgeMappingState,
	) -> FerrosGrynnResult<()> {
		let current = self
			.get(id)
			.ok_or_else(|| FerrosGrynnError::bridge_mapping_error("mapping reference is absent"))?;
		if current.state == state {
			return Ok(());
		}
		if current.state == BridgeMappingState::Tombstoned || state == BridgeMappingState::Pending {
			return Err(FerrosGrynnError::bridge_mapping_error(
				"invalid mapping lifecycle transition",
			));
		}
		let mut row = current.as_ref().clone();
		let domain = (row.external.database.id, row.subject.role());
		row.state = state;
		if state == BridgeMappingState::Tombstoned {
			self.direct.remove(
				&(row.subject.clone(), row.external.database.id),
				row.subject.record().estimated_heap_bytes(),
			);
			self.reverse.remove(
				&(row.external.clone(), row.subject.role()),
				row.external.record.estimated_heap_bytes(),
			);
		}
		self.rows.insert(id, Arc::new(row), 0);
		self.touched.insert(domain);
		self.advance()?;
		Ok(())
	}

	/// Retires matching local identities without deleting native records.
	pub(crate) fn tombstone_subject(
		&mut self,
		subject: &BridgeMappingSubject,
	) -> FerrosGrynnResult<Vec<BridgeDatabaseId>> {
		let targets = self
			.databases
			.keys()
			.filter_map(|database| {
				self.local(*database, subject).map(|row| (*database, row.reference.id))
			})
			.collect::<Vec<_>>();
		let mut databases = Vec::with_capacity(targets.len());
		for (database, id) in targets {
			self.transition(id, BridgeMappingState::Tombstoned)?;
			databases.push(database);
		}
		Ok(databases)
	}

	/// Tombstones the global properties role before atom deletion.
	pub(crate) fn tombstone_atom(
		&mut self,
		record: &RecordId,
	) -> FerrosGrynnResult<Vec<BridgeDatabaseId>> {
		self.tombstone_subject(&BridgeMappingSubject::Atom(record.clone()))
	}

	/// Advances the logical generation with overflow validation.
	fn advance(&mut self) -> FerrosGrynnResult<()> {
		self.generation = self
			.generation
			.checked_add(1)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("bridge registry generation"))?;
		Ok(())
	}
}

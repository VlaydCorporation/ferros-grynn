//! Atomic durable correspondence reservation, lookup and confirmation.

use std::sync::Arc;

use super::{Mutation, Transaction};
use crate::{
	bridge::{
		BridgeDatabase,
		BridgeDatabaseId,
		BridgeDatabaseOptions,
		BridgeDatabaseRef,
		BridgeEntityRef,
		BridgeMapping,
		BridgeMappingId,
		BridgeMappingRef,
		BridgeMappingRole,
		BridgeMappingState,
		BridgeMappingSubject,
		BridgeMirrorRequest,
	},
	datastore::ConflictKey,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::AtomKind,
};

impl Transaction {
	/// Tracks coarse SSI domains before the captured graph recipe is consumed.
	pub(crate) fn query_bridge_recipe_reads(&self, root: &crate::datastore::DatabaseRoot) {
		self.record_read(ConflictKey::BridgeDatabases);
		for database in root.bridges.databases.keys() {
			for role in [BridgeMappingRole::Atom, BridgeMappingRole::Relation] {
				self.record_read(ConflictKey::BridgeMappings(*database, role));
			}
		}
		for class in root.schema.classes() {
			if let Some(id) = class.id() {
				self.record_read(ConflictKey::BaseClassRange(id));
			}
		}
		for graph in root.schema.graphs() {
			self.record_read(ConflictKey::GraphDefinition(graph.reference().id()));
			for kind in [AtomKind::Node, AtomKind::Edge] {
				self.record_read(ConflictKey::GraphKindRange(graph.reference().id(), kind));
			}
		}
	}

	/// Resolves local qualified incidence to the correct mapping role.
	pub(crate) fn bridge_endpoint_subject(
		&self,
		graph: crate::graph::GraphId,
		endpoint: &crate::graph::EdgeEndpoint,
	) -> FerrosGrynnResult<BridgeMappingSubject> {
		let (graph, occurrence) =
			self.working.resolve_incidence_endpoint(graph, endpoint, |graph, owner| {
				self.record_read(ConflictKey::GraphMembership(graph, owner.global_id()));
				Ok(())
			})?;
		self.record_read(ConflictKey::GraphMembership(graph, occurrence.global_id()));
		let atom = self.working.atom(occurrence.global_id()).ok_or_else(|| {
			FerrosGrynnError::bridge_mapping_error("local endpoint atom is absent")
		})?;
		let record = atom.record_id().clone();
		Ok(if atom.kind() == AtomKind::Edge {
			BridgeMappingSubject::Relation {
				graph,
				edge: record,
			}
		} else {
			BridgeMappingSubject::Atom(record)
		})
	}

	/// Tracks the database and role domain in the current snapshot.
	pub(crate) fn query_bridge_domain(&self, database: BridgeDatabaseId, role: BridgeMappingRole) {
		self.record_read(ConflictKey::BridgeDatabases);
		self.record_read(ConflictKey::BridgeMappings(database, role));
	}

	/// Registers durable non-secret metadata without creating an external
	/// client.
	///
	/// # Errors
	/// Returns invalid correspondence, read-only, budget or transaction errors.
	pub fn register_bridge_database(
		&mut self,
		options: BridgeDatabaseOptions,
	) -> FerrosGrynnResult<BridgeDatabaseRef> {
		self.atomic_mutation(|tx| {
			tx.prepare_schema_mutation()?;
			tx.record_read(ConflictKey::BridgeDatabases);
			if tx.working.bridges.aliases.contains_key(&options.alias) {
				return Err(FerrosGrynnError::bridge_mapping_error(
					"bridge alias is already registered",
				));
			}
			for class in options.classes.keys() {
				tx.working.schema.class(class).ok_or_else(|| {
					FerrosGrynnError::bridge_mapping_error("mapped local class is absent")
				})?;
			}
			for graph in options.graphs.keys() {
				tx.working.graph(*graph).ok_or_else(|| {
					FerrosGrynnError::bridge_mapping_error("mapped local graph is absent")
				})?;
			}
			let bytes = 4096_usize
				.checked_add(options.classes.len().saturating_mul(8192))
				.and_then(|bytes| bytes.checked_add(options.graphs.len().saturating_mul(4096)))
				.ok_or_else(|| FerrosGrynnError::operation_overflow("bridge database metadata"))?;
			tx.ensure_capacity(bytes)?;
			let id = BridgeDatabaseId::new(tx.store.inner.allocate_schema_id()?.get()).ok_or_else(
				|| FerrosGrynnError::bridge_mapping_error("bridge identity is reserved"),
			)?;
			let reference = BridgeDatabaseRef {
				datastore: tx.working.database_uuid,
				id,
			};
			let database = Arc::new(BridgeDatabase {
				reference,
				alias: options.alias,
				provider: options.provider,
				namespace: options.namespace,
				database: options.database,
				classes: options.classes,
				graphs: options.graphs,
			});
			crate::storage::validate_database_record(&database)?;
			Arc::make_mut(&mut tx.working.bridges).install_database(database.clone())?;
			tx.insert_write(ConflictKey::BridgeDatabases, 0);
			tx.write_bytes += bytes;
			tx.mutations.push(Mutation::RegisterBridgeDatabase(database));
			Ok(reference)
		})
	}

	/// Reads one registered database from this transaction's immutable view.
	/// # Errors
	/// Returns snapshot-age and transaction lifecycle errors.
	pub fn bridge_database(&mut self, alias: &str) -> FerrosGrynnResult<Option<BridgeDatabase>> {
		self.prepare_read()?;
		self.record_read(ConflictKey::BridgeDatabases);
		Ok(self
			.working
			.bridges
			.aliases
			.get(alias)
			.and_then(|id| self.working.bridges.databases.get(id))
			.map(|value| value.as_ref().clone()))
	}

	/// Returns the authoritative mapping for an owned reference.
	/// # Errors
	/// Rejects foreign datastore references or invalid transaction state.
	pub fn bridge_mapping(
		&mut self,
		reference: BridgeMappingRef,
	) -> FerrosGrynnResult<Option<BridgeMapping>> {
		self.prepare_read()?;
		self.check_bridge_origin(reference.datastore)?;
		let row = self.working.bridges.get(reference.id).cloned();
		if let Some(row) = &row {
			self.record_read(ConflictKey::BridgeMappings(
				row.external.database.id,
				row.subject.role(),
			));
		}
		Ok(row.map(|value| value.as_ref().clone()))
	}

	/// Performs indexed local lookup, including absence tracking.
	/// # Errors
	/// Rejects foreign database references or invalid transaction state.
	pub fn bridge_mapping_local(
		&mut self,
		database: BridgeDatabaseRef,
		subject: &BridgeMappingSubject,
	) -> FerrosGrynnResult<Option<BridgeMapping>> {
		self.prepare_read()?;
		self.check_bridge_database(database)?;
		self.record_read(ConflictKey::BridgeMappings(database.id, subject.role()));
		Ok(self.working.bridges.local(database.id, subject).map(|value| value.as_ref().clone()))
	}

	/// Performs inverse indexed lookup in one database/role domain.
	/// # Errors
	/// Rejects foreign database references or invalid transaction state.
	pub fn bridge_mapping_external(
		&mut self,
		entity: &BridgeEntityRef,
		role: BridgeMappingRole,
	) -> FerrosGrynnResult<Option<BridgeMapping>> {
		self.prepare_read()?;
		self.check_bridge_database(entity.database)?;
		self.record_read(ConflictKey::BridgeMappings(entity.database.id, role));
		Ok(self.working.bridges.external(entity, role).map(|value| value.as_ref().clone()))
	}

	/// Registers an existing one-to-one correspondence in Ready state.
	/// # Errors
	/// Rejects missing local subjects, conflicting sides and transaction
	/// errors.
	pub fn register_bridge_mapping(
		&mut self,
		subject: BridgeMappingSubject,
		external: BridgeEntityRef,
	) -> FerrosGrynnResult<BridgeMappingRef> {
		self.atomic_mutation(|tx| tx.install_bridge_mapping(subject, external, None))
	}

	/// Reserves both directions and an exact mirror before local commit.
	/// # Errors
	/// Rejects conflicting identities, invalid subjects and admission failures.
	pub fn reserve_bridge_mapping(
		&mut self,
		subject: BridgeMappingSubject,
		external: BridgeEntityRef,
		mirror: BridgeMirrorRequest,
	) -> FerrosGrynnResult<BridgeMappingRef> {
		if subject.role() != BridgeMappingRole::Relation || mirror.relation != external.record {
			return Err(FerrosGrynnError::bridge_mapping_error(
				"mirror identity does not match relation mapping",
			));
		}
		self.atomic_mutation(|tx| tx.install_bridge_mapping(subject, external, Some(mirror)))
	}

	/// Atomically validates and reserves both unique correspondence sides.
	fn install_bridge_mapping(
		&mut self,
		subject: BridgeMappingSubject,
		external: BridgeEntityRef,
		mirror: Option<BridgeMirrorRequest>,
	) -> FerrosGrynnResult<BridgeMappingRef> {
		self.prepare_write(false)?;
		self.check_bridge_database(external.database)?;
		let retained = subject
			.record()
			.estimated_heap_bytes()
			.checked_add(external.record.estimated_heap_bytes())
			.and_then(|bytes| bytes.checked_mul(4))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("bridge identity preflight"))?;
		self.ensure_capacity(retained)?;
		crate::bridge::types::validate_record(&external.record)?;
		if let Some(intent) = &mirror {
			crate::bridge::types::validate_record(&intent.source)?;
			crate::bridge::types::validate_record(&intent.target)?;
		}
		self.require_bridge_subject(&subject)?;
		let key = ConflictKey::BridgeMappings(external.database.id, subject.role());
		self.record_read(key);
		self.record_requirement(key);
		if let Some(row) = self.working.bridges.local(external.database.id, &subject) {
			if row.external == external && row.mirror.as_deref() == mirror.as_ref() {
				return Ok(row.reference);
			}
			return Err(FerrosGrynnError::bridge_mapping_error(
				"local mapping side is already reserved",
			));
		}
		if self.working.bridges.external(&external, subject.role()).is_some() {
			return Err(FerrosGrynnError::bridge_mapping_error(
				"external mapping side is already reserved",
			));
		}
		let reference = BridgeMappingRef {
			datastore: self.working.database_uuid,
			id: BridgeMappingId(uuid::Uuid::now_v7()),
		};
		let row = Arc::new(BridgeMapping {
			reference,
			subject,
			external,
			state: if mirror.is_some() {
				BridgeMappingState::Pending
			} else {
				BridgeMappingState::Ready
			},
			mirror: mirror.map(Arc::new),
		});
		let bytes = self.working.bridges.admit_row(&row)?;
		self.ensure_capacity(bytes)?;
		crate::storage::validate_mapping_record(&row)?;
		Arc::make_mut(&mut self.working.bridges).install(row.clone())?;
		self.insert_write(key, 0);
		self.write_bytes += bytes;
		self.mutations.push(Mutation::RegisterBridgeMapping(row));
		Ok(reference)
	}

	/// Confirms a verified external operation without replaying local DML.
	/// # Errors
	/// Rejects missing/tombstoned subjects, stale reservations and transaction
	/// errors.
	pub fn confirm_bridge_mapping(&mut self, reference: BridgeMappingRef) -> FerrosGrynnResult<()> {
		self.atomic_mutation(|tx| {
			tx.prepare_write(false)?;
			tx.check_bridge_origin(reference.datastore)?;
			let row = tx.working.bridges.get(reference.id).cloned().ok_or_else(|| {
				FerrosGrynnError::bridge_mapping_error("mapping reference is absent")
			})?;
			tx.require_bridge_subject(&row.subject)?;
			if row.state == BridgeMappingState::Ready {
				return Ok(());
			}
			let key = ConflictKey::BridgeMappings(row.external.database.id, row.subject.role());
			tx.record_requirement(key);
			let bytes = tx.working.bridges.admit_transition(&row)?;
			tx.ensure_capacity(bytes)?;
			Arc::make_mut(&mut tx.working.bridges)
				.transition(reference.id, BridgeMappingState::Ready)?;
			tx.insert_write(key, 0);
			tx.write_bytes += bytes;
			tx.mutations.push(Mutation::TransitionBridgeMapping {
				reference,
				state: BridgeMappingState::Ready,
			});
			Ok(())
		})
	}

	/// Rejects a reference belonging to another datastore.
	fn check_bridge_origin(&self, origin: uuid::Uuid) -> FerrosGrynnResult<()> {
		if origin != self.working.database_uuid {
			return Err(FerrosGrynnError::bridge_mapping_error(
				"bridge reference belongs to another datastore",
			));
		}
		Ok(())
	}

	/// Validates the database origin and descriptor presence.
	fn check_bridge_database(&self, database: BridgeDatabaseRef) -> FerrosGrynnResult<()> {
		self.check_bridge_origin(database.datastore)?;
		if !self.working.bridges.databases.contains_key(&database.id) {
			return Err(FerrosGrynnError::bridge_mapping_error("bridge database is absent"));
		}
		self.record_read(ConflictKey::BridgeDatabases);
		Ok(())
	}

	/// Validates the local atom or occurrence and retains its commit
	/// requirements.
	fn require_bridge_subject(&mut self, subject: &BridgeMappingSubject) -> FerrosGrynnResult<()> {
		let global = self.require_atom(subject.record())?;
		self.record_requirement(ConflictKey::BaseAtom(global));
		if let BridgeMappingSubject::Relation {
			graph,
			..
		} = subject
		{
			let atom = self.working.atom(global).ok_or_else(|| {
				FerrosGrynnError::bridge_mapping_error("bridge subject atom is absent")
			})?;
			if atom.kind() != AtomKind::Edge || self.working.membership(*graph, global).is_none() {
				return Err(FerrosGrynnError::bridge_mapping_error(
					"relation subject is not a local edge occurrence",
				));
			}
			self.record_requirement(ConflictKey::GraphMembership(*graph, global));
			if let Some(local) = self.working.membership(*graph, global) {
				self.record_requirement(ConflictKey::GraphIncidence(*graph, local));
			}
		}
		Ok(())
	}

	/// Captures the reservation and local subject requirements before external
	/// IO.
	pub(crate) fn prepare_bridge_resume(
		&mut self,
		reference: BridgeMappingRef,
	) -> FerrosGrynnResult<BridgeMapping> {
		let row = self
			.bridge_mapping(reference)?
			.ok_or_else(|| FerrosGrynnError::bridge_mapping_error("mapping reference is absent"))?;
		if row.state != BridgeMappingState::Pending {
			return Ok(row);
		}
		self.require_bridge_subject(&row.subject)?;
		self.record_requirement(ConflictKey::BridgeMappings(
			row.external.database.id,
			row.subject.role(),
		));
		Ok(row)
	}

	/// Charges removed occurrences before graph replacement or cascading
	/// removal.
	pub(super) fn admit_bridge_graph_removal(
		&mut self,
		graph: crate::graph::GraphId,
		retained: Option<&std::collections::BTreeSet<crate::datastore::GlobalAtomId>>,
	) -> FerrosGrynnResult<()> {
		if self.working.bridges.databases.is_empty() {
			return Ok(());
		}
		let Some(root) = self.working.graphs.get(&graph).cloned() else {
			return Ok(());
		};
		for occurrence in root.occurrences() {
			if retained.is_some_and(|ids| ids.contains(&occurrence.global_id())) {
				continue;
			}
			let Some(atom) = self.working.atom(occurrence.global_id()) else {
				continue;
			};
			if atom.kind() != AtomKind::Edge {
				continue;
			}
			self.ensure_capacity(atom.record_id().estimated_heap_bytes())?;
			let subject = BridgeMappingSubject::Relation {
				graph,
				edge: atom.record_id().clone(),
			};
			self.admit_bridge_tombstones(&subject)?;
		}
		Ok(())
	}

	/// Reserves every implicit mapping transition independently of domain keys.
	pub(super) fn admit_bridge_tombstones(
		&mut self,
		subject: &BridgeMappingSubject,
	) -> FerrosGrynnResult<()> {
		let registry = self.working.bridges.clone();
		for row in registry.databases.keys().filter_map(|id| registry.local(*id, subject)) {
			self.ensure_capacity(
				row.subject
					.record()
					.estimated_heap_bytes()
					.saturating_add(row.external.record.estimated_heap_bytes())
					.saturating_mul(4),
			)?;
			let bytes = self.working.bridges.admit_transition(&row)?;
			self.ensure_capacity(bytes)?;
			let key = ConflictKey::BridgeMappings(row.external.database.id, row.subject.role());
			self.record_requirement(key);
			self.insert_write(key, 0);
			self.write_bytes += bytes;
		}
		Ok(())
	}
}

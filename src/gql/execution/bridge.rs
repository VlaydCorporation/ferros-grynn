//! Snapshot-bound bridge reads; async clients remain behind the bounded port.

use std::sync::Arc;

use super::{ExecutionControl, QueryRead, Snapshot};
use crate::{
	bridge::{
		BridgeDatabase,
		BridgeEntityRef,
		BridgeMappingRef,
		BridgeMappingRole,
		BridgeMappingState,
		BridgeMappingSubject,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomKind, GraphRef},
	values::{Array, Object, RecordId, Value},
};

/// Resolves the named runtime adapter against its durable descriptor.
pub(super) fn resolve<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	alias: &str,
	control: &ExecutionControl,
	mirror: bool,
) -> FerrosGrynnResult<Arc<BridgeDatabase>> {
	let scope = control.bridge_scope.as_ref().ok_or_else(|| {
		FerrosGrynnError::bridge_operation_error(
			"WITH BRIDGE requires Datastore::query orchestration",
		)
	})?;
	let (database, capabilities) = scope.resolve(alias)?;
	let root = read.snapshot_root(snapshot)?;
	if root.bridges.databases.get(&database.reference.id).map(Arc::as_ref)
		!= Some(database.as_ref())
	{
		return Err(FerrosGrynnError::bridge_mapping_error(
			"adapter metadata differs from statement snapshot",
		));
	}
	if mirror && !capabilities.mirror || !mirror && !capabilities.records {
		return Err(FerrosGrynnError::bridge_operation_error(
			"adapter lacks the requested bridge capability",
		));
	}
	Ok(database)
}

/// Materializes the owned mapping reference as typed UUID fields.
pub(super) fn reference_value(reference: BridgeMappingRef) -> Value {
	reference.into()
}

/// Selects a Ready occurrence mapping or its global-atom fallback.
fn fetch_mapping<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	record: &RecordId,
	graph: Option<&GraphRef>,
	database: crate::bridge::BridgeDatabaseRef,
) -> FerrosGrynnResult<crate::bridge::BridgeMapping> {
	let relation = graph.map(|graph| BridgeMappingSubject::Relation {
		graph: graph.id(),
		edge: record.clone(),
	});
	let mapping = relation
		.as_ref()
		.map(|subject| read.bridge_mapping(snapshot, database, subject))
		.transpose()?
		.flatten();
	let mapping = match mapping {
		Some(mapping) => Some(mapping),
		None => {
			read.bridge_mapping(snapshot, database, &BridgeMappingSubject::Atom(record.clone()))?
		}
	}
	.ok_or_else(|| FerrosGrynnError::bridge_mapping_error("FETCH mapping is absent"))?;
	if mapping.state != BridgeMappingState::Ready {
		return Err(FerrosGrynnError::bridge_mapping_error("FETCH mapping is not Ready"));
	}
	Ok(mapping)
}

/// Collection reads send one exact batch and retain a query-scoped cache.
pub(super) fn prefetch<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	values: &Value,
	graph: Option<&GraphRef>,
	alias: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<()> {
	let database = resolve(read, snapshot, alias, control, false)?;
	let scope = control
		.bridge_scope
		.as_ref()
		.ok_or_else(|| FerrosGrynnError::bridge_operation_error("bridge scope is absent"))?;
	scope.context.admit_value(values)?;
	let root = read.snapshot_root(snapshot)?;
	let mut pending = vec![values];
	let mut records = Vec::new();
	while let Some(value) = pending.pop() {
		control.check()?;
		match value {
			Value::Array(values) => pending.extend(values.iter()),
			Value::Set(values) => pending.extend(values.iter()),
			Value::Record(record) => {
				if root.atom_by_record(record).is_some() {
					records.push(
						fetch_mapping(read, snapshot, record, graph, database.reference)?
							.external
							.record,
					);
				}
			}
			_ => {}
		}
	}
	scope.prefetch(database.reference.id, records, control)
}

/// Adds verified external namespaces while preserving the local record fields.
pub(super) fn fetch<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	record: &RecordId,
	graph: Option<&GraphRef>,
	alias: &str,
	mut local: Object,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	let database = resolve(read, snapshot, alias, control, false)?;
	let mapping = fetch_mapping(read, snapshot, record, graph, database.reference)?;
	if read.catalog(snapshot).schema(record.class())?.index_of("external").is_some() {
		return Err(FerrosGrynnError::bridge_operation_error(
			"local FETCH field external is reserved by WITH BRIDGE",
		));
	}
	let scope = control
		.bridge_scope
		.as_ref()
		.ok_or_else(|| FerrosGrynnError::bridge_operation_error("bridge scope is absent"))?;
	if let Some(external) = local.get("external") {
		let Value::Object(entries) = external else {
			return Err(FerrosGrynnError::bridge_operation_error(
				"local field external conflicts with FETCH namespace",
			));
		};
		for (name, entry) in entries {
			let (previous, _) = scope.resolve(name.as_str())?;
			let expected = fetch_mapping(read, snapshot, record, graph, previous.reference)?;
			let Value::Object(entry) = entry else {
				return Err(FerrosGrynnError::bridge_operation_error(
					"local field external conflicts with FETCH namespace",
				));
			};
			let expected_database = Value::Object(Object::from_iter([
				("datastore", Value::Uuid(previous.reference.datastore.into())),
				("id", Value::from(previous.reference.id.get())),
			]));
			if entry.len() != 3
				|| entry.get("database") != Some(&expected_database)
				|| entry.get("mapping") != Some(&reference_value(expected.reference))
				|| scope.cached_record(previous.reference.id, &expected.external.record).as_deref()
					!= entry.get("properties")
			{
				return Err(FerrosGrynnError::bridge_operation_error(
					"local field external conflicts with FETCH namespace",
				));
			}
		}
	}
	let properties = scope.records(database.reference.id, &mapping.external.record, control)?;
	scope.context.admit_value(properties.as_ref())?;
	let Value::Object(properties) = properties.as_ref() else {
		return Err(FerrosGrynnError::bridge_operation_error(
			"external FETCH record is missing or not an Object",
		));
	};
	let entry = Value::Object(Object::from_iter([
		(
			"database",
			Value::Object(Object::from_iter([
				("datastore", Value::Uuid(database.reference.datastore.into())),
				("id", Value::from(database.reference.id.get())),
			])),
		),
		("mapping", reference_value(mapping.reference)),
		("properties", Value::Object(properties.clone())),
	]));
	let external = local.remove("external").unwrap_or_else(|| Value::Object(Object::new()));
	let Value::Object(mut external) = external else {
		return Err(FerrosGrynnError::bridge_operation_error(
			"external FETCH namespace is not an Object",
		));
	};
	external.insert(alias, entry);
	local.insert("external", Value::Object(external));
	Ok(Value::Object(local))
}

/// Converts only identities; native relation objects never enter the assembler.
pub(super) fn recipes<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	value: Value,
	alias: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	let database = resolve(read, snapshot, alias, control, false)?;
	match value {
		Value::Array(values) => values
			.into_iter()
			.map(|value| recipes(read, snapshot, value, alias, control))
			.collect::<FerrosGrynnResult<Array>>()
			.map(Value::Array),
		Value::Set(values) => values
			.into_iter()
			.map(|value| recipes(read, snapshot, value, alias, control))
			.collect::<FerrosGrynnResult<crate::values::Set>>()
			.map(Value::Set),
		Value::None => Ok(Value::None),
		Value::Record(record) => {
			let row = read
				.bridge_inverse(
					snapshot,
					&BridgeEntityRef {
						database: database.reference,
						record: *record,
					},
					BridgeMappingRole::Atom,
				)?
				.ok_or_else(|| {
					FerrosGrynnError::bridge_mapping_error(
						"external atom requires explicit import and mapping",
					)
				})?;
			if row.state != BridgeMappingState::Ready {
				return Err(FerrosGrynnError::bridge_mapping_error("recipe mapping is not Ready"));
			}
			Ok(Value::Record(Box::new(row.subject.record().clone())))
		}
		Value::Object(mut object) => {
			let id = object.remove("id").ok_or_else(|| {
				FerrosGrynnError::bridge_operation_error("relation recipe id is absent")
			})?;
			let Value::Record(id) = id else {
				return Err(FerrosGrynnError::bridge_operation_error(
					"relation recipe id must be a typed RecordId",
				));
			};
			let mapping = read
				.bridge_inverse(
					snapshot,
					&BridgeEntityRef {
						database: database.reference,
						record: *id,
					},
					BridgeMappingRole::Relation,
				)?
				.ok_or_else(|| {
					FerrosGrynnError::bridge_mapping_error(
						"external relation requires explicit local mapping",
					)
				})?;
			let BridgeMappingSubject::Relation {
				graph,
				edge,
			} = &mapping.subject
			else {
				return Err(FerrosGrynnError::bridge_mapping_error(
					"relation mapping role mismatch",
				));
			};
			if mapping.state != BridgeMappingState::Ready {
				return Err(FerrosGrynnError::bridge_mapping_error("recipe mapping is not Ready"));
			}
			let root = read.snapshot_root(snapshot)?;
			let reference = root
				.schema
				.graph(*graph)
				.ok_or_else(|| FerrosGrynnError::bridge_mapping_error("recipe graph is absent"))?
				.reference()
				.clone();
			let mut endpoints = Vec::new();
			for name in ["in", "out"] {
				let Some(Value::Record(endpoint)) = object.remove(name) else {
					return Err(FerrosGrynnError::bridge_operation_error(
						"relation endpoint must be a typed RecordId",
					));
				};
				let entity = BridgeEntityRef {
					database: database.reference,
					record: *endpoint,
				};
				let atom = read.bridge_inverse(snapshot, &entity, BridgeMappingRole::Atom)?;
				let relation =
					read.bridge_inverse(snapshot, &entity, BridgeMappingRole::Relation)?;
				if atom.as_ref().zip(relation.as_ref()).is_some_and(|(atom, relation)| {
					atom.subject.record() != relation.subject.record()
				}) {
					return Err(FerrosGrynnError::bridge_mapping_error(
						"external endpoint has ambiguous atom/relation roles",
					));
				}
				let mapping = relation.or(atom).ok_or_else(|| {
					FerrosGrynnError::bridge_mapping_error(
						"relation endpoint requires explicit import and mapping",
					)
				})?;
				if mapping.state != BridgeMappingState::Ready {
					return Err(FerrosGrynnError::bridge_mapping_error(
						"endpoint mapping is not Ready",
					));
				}
				if read.base_atom(snapshot, mapping.subject.record())?.is_none() {
					return Err(FerrosGrynnError::bridge_mapping_error(
						"relation endpoint requires explicit import",
					));
				}
				let occurrence = match mapping.subject {
					BridgeMappingSubject::Atom(record) => (record, None),
					BridgeMappingSubject::Relation {
						graph,
						edge,
					} => (edge, Some(graph)),
				};
				endpoints.push(occurrence);
			}
			let atom = root.atom_by_record(edge).ok_or_else(|| {
				FerrosGrynnError::bridge_mapping_error("recipe edge requires explicit import")
			})?;
			let local = root.membership(*graph, atom.id()).ok_or_else(|| {
				FerrosGrynnError::bridge_mapping_error("recipe edge occurrence is absent")
			})?;
			let occurrence = root.occurrence(*graph, local).ok_or_else(|| {
				FerrosGrynnError::bridge_mapping_error("recipe edge occurrence is absent")
			})?;
			let crate::datastore::OccurrenceState::Edge {
				topology,
				..
			} = occurrence.state()
			else {
				return Err(FerrosGrynnError::bridge_mapping_error(
					"recipe subject is not an edge",
				));
			};
			let mut expected = topology
				.endpoints()
				.map(|endpoint| {
					let (graph, occurrence) =
						root.resolve_incidence_endpoint(*graph, endpoint, |_, _| Ok(()))?;
					root.atom(occurrence.global_id())
						.map(|atom| (atom.record_id().clone(), graph))
						.ok_or_else(|| {
							FerrosGrynnError::bridge_mapping_error(
								"recipe incidence atom is absent",
							)
						})
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			if expected.len() == 1 {
				expected.push(expected[0].clone());
			}
			if expected.len() != endpoints.len()
				|| expected.iter().zip(&endpoints).any(|((record, graph), (endpoint, scope))| {
					record != endpoint || scope.is_some_and(|scope| scope != *graph)
				}) {
				return Err(FerrosGrynnError::bridge_mapping_error(
					"external relation recipe conflicts with local incidence",
				));
			}
			Ok(Value::AtomOccurrence(Box::new(crate::graph::AtomOccurrenceRef::new(
				crate::graph::AtomRef::new(AtomKind::Edge, edge.clone()),
				Arc::new(reference),
			))))
		}
		_ => Err(FerrosGrynnError::bridge_operation_error(
			"bridge graph recipe must contain typed identities",
		)),
	}
}

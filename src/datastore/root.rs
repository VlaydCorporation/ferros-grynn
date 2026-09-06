use std::{collections::BTreeMap, sync::Arc, time::Instant};

use super::{
	GraphSchema,
	ReadKey,
	SchemaCatalog,
	SchemaId,
	SchemaParameter,
	SchemaVersion,
	SystemIndex,
	SystemIndexKind,
	TableField,
	TableIndex,
	TableSchema,
};
use crate::{
	collections::cow_vec::CowVec,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::{LocalDatabaseSnapshot, LocalGraph},
	values::runtime::{
		PropertyDictionary,
		PropertyKeyId,
		RecordId,
		graph::{AtomId, AtomKind, GraphKind, GraphRef, GraphStorageMode, MaterializedGraph},
	},
};

pub(crate) struct ActiveSnapshot {
	pub(crate) start_ts: u64,
	pub(crate) started: Instant,
}

#[derive(Clone)]
pub(crate) struct DatabaseRoot {
	pub(crate) commit_ts: u64,
	pub(crate) graphs: BTreeMap<RecordId, StoredGraph>,
	pub(crate) schema: Arc<SchemaCatalog>,
	pub(crate) dictionary: Arc<PropertyDictionary>,
}

#[derive(Clone)]
pub(crate) struct StoredGraph {
	/// Durable top-level catalog name.
	pub(crate) name: String,
	pub(crate) graph: StoredGraphValue,
	pub(crate) record_index: Arc<BTreeMap<String, AtomId>>,
	pub(crate) last_commit_ts: u64,
	pub(crate) properties_version: u64,
	pub(crate) node_set_version: u64,
	pub(crate) edge_set_version: u64,
	pub(crate) atom_versions: AtomVersions,
	pub(crate) changes: CowVec<(u64, AtomId)>,
}

#[derive(Clone)]
pub(crate) enum StoredGraphValue {
	Materialized(Arc<MaterializedGraph>),
	Local(Arc<LocalGraph>),
}

impl StoredGraphValue {
	pub(crate) fn kind(&self) -> GraphKind {
		match self {
			Self::Materialized(graph) => graph.kind(),
			Self::Local(graph) => graph.kind(),
		}
	}

	pub(crate) fn materialized(&self) -> Option<&Arc<MaterializedGraph>> {
		match self {
			Self::Materialized(graph) => Some(graph),
			Self::Local(_) => None,
		}
	}
}

#[derive(Clone, Default)]
pub(crate) struct AtomVersions {
	nodes: CowVec<VersionHistory>,
	edges: CowVec<VersionHistory>,
}

#[derive(Clone, Copy)]
struct VersionEntry {
	generation: u32,
	commit_ts: u64,
}

#[allow(clippy::rc_buffer, reason = "the size changes dynamically during insertion")]
#[derive(Clone, Default)]
struct VersionHistory {
	latest_generation: u32,
	latest_commit_ts: u64,
	older: Option<Arc<Vec<VersionEntry>>>,
}

impl VersionHistory {
	fn get(&self, generation: u32) -> u64 {
		if self.latest_commit_ts != 0 && self.latest_generation == generation {
			return self.latest_commit_ts;
		}
		self.older
			.as_deref()
			.and_then(|entries| entries.iter().rev().find(|entry| entry.generation == generation))
			.map_or(0, |entry| entry.commit_ts)
	}

	fn insert(&mut self, generation: u32, commit_ts: u64) {
		if self.latest_commit_ts == 0 || self.latest_generation == generation {
			self.latest_generation = generation;
			self.latest_commit_ts = commit_ts;
			return;
		}
		Arc::make_mut(self.older.get_or_insert_with(|| Arc::new(Vec::new()))).push(VersionEntry {
			generation: self.latest_generation,
			commit_ts: self.latest_commit_ts,
		});
		self.latest_generation = generation;
		self.latest_commit_ts = commit_ts;
	}
}

impl AtomVersions {
	pub(crate) fn get(&self, atom: AtomId) -> u64 {
		let Ok(slot) = usize::try_from(atom.slot()) else {
			return 0;
		};
		match atom.kind() {
			Some(AtomKind::Node) => self.nodes.get(slot),
			Some(AtomKind::Edge) => self.edges.get(slot),
			None => None,
		}
		.map_or(0, |history| history.get(atom.generation()))
	}

	pub(crate) fn insert(&mut self, atom: AtomId, commit_ts: u64) {
		let Ok(slot) = usize::try_from(atom.slot()) else {
			return;
		};
		let Some(required_len) = slot.checked_add(1) else {
			return;
		};
		let slots = match atom.kind() {
			Some(AtomKind::Node) => &mut self.nodes,
			Some(AtomKind::Edge) => &mut self.edges,
			None => return,
		};
		if slots.len() < required_len {
			slots.resize_with(required_len, VersionHistory::default);
		}
		slots[slot].insert(atom.generation(), commit_ts);
	}
}

impl Default for DatabaseRoot {
	fn default() -> Self {
		Self {
			commit_ts: 0,
			graphs: BTreeMap::new(),
			schema: Arc::new(SchemaCatalog::default()),
			dictionary: Arc::new(PropertyDictionary::new()),
		}
	}
}

impl DatabaseRoot {
	/// Rebuilds the in-process MVCC metadata around a recovered durable root.
	pub(crate) fn from_local(snapshot: LocalDatabaseSnapshot) -> FerrosGrynnResult<Self> {
		let LocalDatabaseSnapshot {
			commit_ts,
			next_schema_id,
			property_dictionary,
			graphs,
			tables,
			parameters,
		} = snapshot;
		let dictionary = property_dictionary;
		let mut catalog = SchemaCatalog {
			next_id: next_schema_id,
			..SchemaCatalog::default()
		};
		let mut root = Self {
			commit_ts,
			graphs: BTreeMap::new(),
			schema: Arc::new(SchemaCatalog::default()),
			dictionary,
		};
		for entry in graphs {
			if root.graphs.contains_key(&entry.record_id) {
				return Err(FerrosGrynnError::invalid_storage_file(
					"durable graph catalog contains duplicate RecordId values",
				));
			}
			if catalog.graph_names.insert(entry.name.clone(), entry.record_id.clone()).is_some() {
				return Err(FerrosGrynnError::invalid_storage_file(
					"durable graph catalog contains duplicate names",
				));
			}
			let graph_schema = GraphSchema::new(
				SchemaId::from_storage(entry.schema.id)?,
				SchemaVersion::from_storage(entry.schema.version)?,
				entry.name.clone().into(),
				entry.graph.reference().clone(),
				SystemIndex::from_storage(
					SchemaId::from_storage(entry.schema.label_index.id)?,
					SchemaVersion::from_storage(entry.schema.label_index.version)?,
					SystemIndexKind::Label,
				),
				SystemIndex::from_storage(
					SchemaId::from_storage(entry.schema.adjacency_index.id)?,
					SchemaVersion::from_storage(entry.schema.adjacency_index.version)?,
					SystemIndexKind::Adjacency,
				),
				commit_ts,
			);
			if catalog.graphs.insert(entry.record_id.clone(), Arc::new(graph_schema)).is_some() {
				return Err(FerrosGrynnError::invalid_storage_file(
					"durable graph catalog contains duplicate RecordId values",
				));
			}
			catalog.graph_versions.insert(entry.record_id.clone(), commit_ts);
			for membership in entry.memberships {
				let table = membership.table_id.map(SchemaId::from_storage).transpose()?;
				catalog.set_membership(&entry.record_id, membership.id, table)?;
			}
			root.graphs.insert(entry.record_id, StoredGraph {
				name: entry.name,
				graph: StoredGraphValue::Local(entry.graph),
				record_index: Arc::new(BTreeMap::new()),
				last_commit_ts: commit_ts,
				properties_version: commit_ts,
				node_set_version: commit_ts,
				edge_set_version: commit_ts,
				atom_versions: AtomVersions::default(),
				changes: CowVec::new(),
			});
		}
		for table in tables {
			let stored = root.graphs.get(&table.graph).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"durable table references a graph absent from the recovered root",
				)
			})?;
			let durable_indexes = table.indexes;
			let fields = table
				.fields
				.into_iter()
				.map(|field| {
					let key = PropertyKeyId::new(field.property_key);
					if root.dictionary.get(&field.name) != Some(key) {
						return Err(FerrosGrynnError::invalid_storage_file(
							"durable table field disagrees with the property dictionary",
						));
					}
					Ok(TableField {
						id: Some(SchemaId::from_storage(field.id)?),
						version: SchemaVersion::from_storage(field.version)?,
						name: field.name.into(),
						value_type: field.value_type,
						key: Some(key),
					})
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			let name = table.name;
			let mut table_schema = TableSchema {
				id: Some(SchemaId::from_storage(table.id)?),
				version: SchemaVersion::from_storage(table.version)?,
				name: name.clone().into(),
				graph: GraphRef::new(
					table.graph,
					stored.graph.kind(),
					GraphStorageMode::DiskBacked,
				),
				kind: table.kind,
				fields,
				indexes: Vec::new(),
				primary_index: Some(SystemIndex::from_storage(
					SchemaId::from_storage(table.primary_index.id)?,
					SchemaVersion::from_storage(table.primary_index.version)?,
					SystemIndexKind::Primary,
				)),
				published_at: commit_ts,
			};
			for index in durable_indexes {
				let field_names = index
					.fields
					.iter()
					.map(|property_key| {
						table_schema
							.fields
							.iter()
							.find(|field| field.key.map(PropertyKeyId::get) == Some(*property_key))
							.map(|field| field.name.clone())
							.ok_or_else(|| {
								FerrosGrynnError::invalid_storage_file(
									"durable scalar index references an unknown table field",
								)
							})
					})
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
				let mut definition = TableIndex::new(index.name, index.kind, field_names)?;
				definition.id = Some(SchemaId::from_storage(index.id)?);
				definition.version = SchemaVersion::from_storage(index.version)?;
				table_schema = table_schema.with_index(definition)?;
			}
			if catalog.tables.insert(name.clone(), Arc::new(table_schema)).is_some() {
				return Err(FerrosGrynnError::invalid_storage_file(
					"durable table catalog contains duplicate names",
				));
			}
			catalog.table_versions.insert(name, commit_ts);
		}
		for parameter in parameters {
			let name = parameter.name;
			let mut definition = SchemaParameter::new(name.clone(), parameter.value)?;
			definition.id = Some(SchemaId::from_storage(parameter.id)?);
			definition.version = SchemaVersion::from_storage(parameter.version)?;
			definition.published_at = commit_ts;
			if catalog.parameters.insert(name.clone(), Arc::new(definition)).is_some() {
				return Err(FerrosGrynnError::invalid_storage_file(
					"durable parameter catalog contains duplicate names",
				));
			}
			catalog.parameter_versions.insert(name, commit_ts);
		}
		catalog.validate_memberships()?;
		root.schema = Arc::new(catalog);
		Ok(root)
	}

	pub(crate) fn version(&self, key: &ReadKey) -> u64 {
		match key {
			ReadKey::GraphCatalog(graph) => {
				self.schema.graph_versions.get(graph).copied().unwrap_or(0)
			}
			ReadKey::GraphProperties(graph) => self.graphs.get(graph).map_or_else(
				|| self.schema.graph_versions.get(graph).copied().unwrap_or(0),
				|stored| stored.properties_version,
			),
			ReadKey::NodeSet(graph) => self.graphs.get(graph).map_or_else(
				|| self.schema.graph_versions.get(graph).copied().unwrap_or(0),
				|stored| stored.node_set_version,
			),
			ReadKey::EdgeSet(graph) => self.graphs.get(graph).map_or_else(
				|| self.schema.graph_versions.get(graph).copied().unwrap_or(0),
				|stored| stored.edge_set_version,
			),
			ReadKey::Atom(graph, atom) => self.graphs.get(graph).map_or_else(
				|| self.schema.graph_versions.get(graph).copied().unwrap_or(0),
				|stored| stored.atom_versions.get(*atom),
			),
			ReadKey::Table(table) => self.schema.table_versions.get(table).copied().unwrap_or(0),
			ReadKey::Parameter(parameter) => {
				self.schema.parameter_versions.get(parameter).copied().unwrap_or(0)
			}
		}
	}
}

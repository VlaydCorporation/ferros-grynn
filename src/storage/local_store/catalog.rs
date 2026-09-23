//! Conversion between the MVCC schema root and exact `FGDC v7` entries.

use std::{collections::BTreeMap, sync::Arc};

use super::GraphBinding;
use crate::{
	datastore::{
		ClassField,
		ClassIndex,
		ClassSchema,
		DatabaseRoot,
		IndexId,
		SchemaCatalog,
		SchemaParameter,
		SystemIndex,
		SystemIndexKind,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{GraphRef, GraphStorageMode},
	storage::{
		atom_store_image::value_context,
		catalog::{
			AtomStoreCatalogBinding,
			ClassCatalogEntry,
			ClassFieldCatalogEntry,
			ClassIndexCatalogEntry,
			DatabaseCatalog,
			GraphCatalogEntry,
			ParameterCatalogEntry,
			PhysicalIndexBinding,
		},
	},
	values::{
		PropertyDictionary,
		storage::{
			EncodeContext,
			StorageValue,
			codec::{CanonicalCodec, DecodeLimits},
		},
	},
};

pub(super) fn encode_database_catalog(
	root: &DatabaseRoot,
	atom_store: AtomStoreCatalogBinding,
	graphs: &[GraphBinding],
	physical_indexes: &BTreeMap<IndexId, PhysicalIndexBinding>,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<DatabaseCatalog> {
	let classes = root
		.schema
		.classes()
		.map(|class| {
			let class_id = class.id().ok_or_else(|| invalid("published class has no ClassId"))?;
			let fields = class
				.fields()
				.map(|field| {
					Ok(ClassFieldCatalogEntry {
						id: field.id().ok_or_else(|| invalid("published field has no SchemaId"))?,
						version: field.version(),
						name: field.name().clone(),
						value_type: field.value_type().clone(),
						property_key: field
							.key
							.ok_or_else(|| invalid("published field has no PropertyKeyId"))?,
					})
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			let indexes = class
				.indexes()
				.map(|index| {
					let id = index.id().ok_or_else(|| invalid("published index has no IndexId"))?;
					let keys = index
						.fields()
						.map(|name| {
							class
								.fields()
								.find(|field| field.name() == name)
								.and_then(|field| field.key)
								.ok_or_else(|| invalid("index field has no durable property key"))
						})
						.collect::<FerrosGrynnResult<Vec<_>>>()?;
					Ok(ClassIndexCatalogEntry {
						id,
						version: index.version(),
						name: index.name().clone(),
						kind: index.kind(),
						state: index.state(),
						scope: index.scope(),
						fields: keys,
						physical: physical_indexes.get(&id).copied(),
					})
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			Ok(ClassCatalogEntry {
				id: class_id,
				version: class.version(),
				published_at: class.published_at,
				name: class.name().clone(),
				kind: class.kind(),
				mode: class.schema_mode(),
				fields,
				indexes,
			})
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;

	let graphs = graphs
		.iter()
		.map(|binding| {
			let schema = root
				.schema
				.graph(binding.graph_id)
				.ok_or_else(|| invalid("physical graph has no schema definition"))?;
			let system = |kind| {
				schema
					.system_indexes()
					.find(|index| index.kind() == kind)
					.copied()
					.ok_or_else(|| invalid("graph system index is absent"))
			};
			let label = system(SystemIndexKind::Label)?;
			let adjacency = system(SystemIndexKind::Adjacency)?;
			Ok(GraphCatalogEntry {
				id: binding.graph_id,
				version: schema.version(),
				published_at: schema.published_at,
				generation: root.commit_ts,
				name: schema.name().clone(),
				kind: schema.reference().kind(),
				storage_mode: GraphStorageMode::DiskBacked,
				label_index: (label.id(), label.version()),
				adjacency_index: (adjacency.id(), adjacency.version()),
				membership: binding.membership,
				arena: binding.arena.clone(),
			})
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;

	let parameters = root
		.schema
		.parameters()
		.map(|parameter| {
			let storage = StorageValue::from_runtime(parameter.value().clone(), context)?;
			Ok(ParameterCatalogEntry {
				id: parameter.id().ok_or_else(|| invalid("published parameter has no SchemaId"))?,
				version: parameter.version(),
				published_at: parameter.published_at,
				name: parameter.name().clone(),
				value: CanonicalCodec::encode(storage.as_ref())?,
			})
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;

	Ok(DatabaseCatalog {
		next_graph_id: root.schema.next_graph_id,
		next_schema_id: root.schema.next_schema_id,
		time_zones: context.time_zone_names().cloned().collect(),
		atom_store,
		classes,
		graphs,
		parameters,
	})
}

pub(super) fn decode_schema(catalog: &DatabaseCatalog) -> FerrosGrynnResult<SchemaCatalog> {
	let mut schema = SchemaCatalog::default();
	schema.next_schema_id = catalog.next_schema_id;
	schema.next_graph_id = catalog.next_graph_id;
	for entry in &catalog.classes {
		let fields = entry
			.fields
			.iter()
			.map(|field| ClassField {
				id: Some(field.id),
				version: field.version,
				class_id: Some(entry.id),
				name: field.name.clone(),
				value_type: field.value_type.clone(),
				key: Some(field.property_key),
			})
			.collect::<Vec<_>>();
		let indexes = entry
			.indexes
			.iter()
			.map(|index| {
				let names = index
					.fields
					.iter()
					.map(|key| {
						entry
							.fields
							.iter()
							.find(|field| field.property_key == *key)
							.map(|field| field.name.clone())
							.ok_or_else(|| invalid("catalog index references an absent field"))
					})
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
				Ok(ClassIndex {
					id: Some(index.id),
					version: index.version,
					name: index.name.clone(),
					kind: index.kind,
					state: index.state,
					scope: index.scope,
					fields: names,
				})
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let class = Arc::new(ClassSchema {
			id: Some(entry.id),
			version: entry.version,
			name: entry.name.clone(),
			kind: entry.kind,
			mode: entry.mode,
			fields,
			indexes,
			published_at: entry.published_at,
		});
		schema.class_names.insert(entry.name.to_string(), entry.id);
		schema.classes.insert(entry.id, class);
		schema.class_versions.insert(entry.id, entry.published_at);
	}
	for entry in &catalog.graphs {
		let reference =
			GraphRef::new(entry.id, entry.name.clone(), entry.kind, GraphStorageMode::DiskBacked)?;
		let graph = Arc::new(crate::datastore::GraphSchema::new(
			entry.version,
			reference,
			SystemIndex::from_storage(
				entry.label_index.0,
				entry.label_index.1,
				SystemIndexKind::Label,
			),
			SystemIndex::from_storage(
				entry.adjacency_index.0,
				entry.adjacency_index.1,
				SystemIndexKind::Adjacency,
			),
			entry.published_at,
		));
		schema.graph_names.insert(entry.name.to_string(), entry.id);
		schema.graphs.insert(entry.id, graph);
		schema.graph_versions.insert(entry.id, entry.published_at);
	}
	Ok(schema)
}

pub(super) fn decode_parameters(
	catalog: &DatabaseCatalog,
	dictionary: &PropertyDictionary,
	schema: &mut SchemaCatalog,
) -> FerrosGrynnResult<()> {
	let mut context = value_context(schema, dictionary.clone())?;
	for zone in &catalog.time_zones {
		context.register_time_zone_name(zone.clone())?;
	}
	for entry in &catalog.parameters {
		let value = CanonicalCodec::decode(&entry.value, DecodeLimits::default())?
			.into_runtime(&context.decoder())?;
		let parameter = Arc::new(SchemaParameter {
			id: Some(entry.id),
			version: entry.version,
			name: entry.name.clone(),
			value,
			published_at: entry.published_at,
		});
		schema.parameters.insert(entry.name.to_string(), parameter);
		schema.parameter_versions.insert(entry.name.to_string(), entry.published_at);
	}
	Ok(())
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

//! Conversion between the MVCC schema root and exact `FGDC v12` entries.

use std::{collections::BTreeMap, sync::Arc};

use super::GraphBinding;
use crate::{
	datastore::{
		AnalyzerFilter,
		ClassField,
		ClassIndex,
		ClassSchema,
		DatabaseRoot,
		IndexId,
		SchemaAnalyzer,
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
			AnalyzerCatalogEntry,
			AnalyzerFilterCatalogEntry,
			AtomStoreCatalogBinding,
			ChangefeedCatalogEntry,
			ClassCatalogEntry,
			ClassFieldCatalogEntry,
			ClassIndexCatalogEntry,
			DatabaseCatalog,
			FunctionCatalogEntry,
			GraphCatalogEntry,
			GraphPropertyCatalogEntry,
			LiveBindingCatalogEntry,
			LiveCatalogEntry,
			LiveHistoryCatalogEntry,
			ParameterCatalogEntry,
			PhysicalIndexBinding,
		},
	},
	values::{
		Object,
		PropertyDictionary,
		Value,
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
	physical_deltas: &BTreeMap<IndexId, PhysicalIndexBinding>,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<DatabaseCatalog> {
	// Catalog DTOs share values/recipes but own their identity vectors. Admit
	// the aggregate conversion before collect or copying any such vectors.
	let mut live_budget = crate::storage::history_codec::CodecBudget::new(256 * 1024 * 1024, 0)?;
	for (id, history) in &root.live_catalog().histories {
		let bytes = history
			.epochs
			.len()
			.checked_mul(std::mem::size_of::<crate::datastore::LiveCaptureEpoch>())
			.and_then(|bytes| {
				bytes.checked_add(
					root.live_segments()
						.get(id)
						.map_or(0, |segments| segments.len())
						.checked_mul(std::mem::size_of::<crate::storage::HistorySegment>())?,
				)
			})
			.and_then(|bytes| {
				bytes.checked_add(std::mem::size_of::<LiveHistoryCatalogEntry>() + 512)
			})
			.ok_or_else(|| invalid("LIVE history catalog metadata overflows"))?;
		live_budget.admit(bytes)?;
	}
	for entry in root.live_catalog().bindings.values() {
		let diagnostic_bytes = match entry.diagnostic.as_ref().map(|error| error.status().status())
		{
			Some(crate::gql_status::Status::LiveEvaluationFault {
				origin_code,
				reason,
				..
			}) => origin_code
				.len()
				.checked_add(reason.len())
				.and_then(|bytes| bytes.checked_mul(2))
				.ok_or_else(|| invalid("LIVE fault metadata overflows"))?,
			_ => 0,
		};
		let bytes = entry
			.dependencies
			.len()
			.checked_mul(std::mem::size_of::<crate::live::LiveBindingId>())
			.and_then(|bytes| {
				bytes.checked_add(entry.functions.len().checked_mul(std::mem::size_of::<(
					crate::datastore::FunctionId,
					crate::datastore::SchemaVersion,
				)>())?)
			})
			.and_then(|bytes| {
				bytes.checked_add(entry.record_sources.len().checked_mul(std::mem::size_of::<(
					crate::values::Text,
					crate::datastore::ClassId,
				)>())?)
			})
			.and_then(|bytes| {
				bytes.checked_add(std::mem::size_of::<LiveBindingCatalogEntry>() + 512)
			})
			.and_then(|bytes| bytes.checked_add(diagnostic_bytes))
			.ok_or_else(|| invalid("LIVE binding catalog metadata overflows"))?;
		live_budget.admit(bytes)?;
		for (name, _) in &entry.record_sources {
			live_budget.admit(name.len())?;
		}
	}
	for entry in root.live_catalog().queries.values() {
		live_budget.admit(crate::datastore::LiveRhsScopeAnchor::retained_bytes(
			&entry.rhs_scope_anchors,
		)?)?;
		let bytes = entry
			.functions
			.len()
			.checked_mul(std::mem::size_of::<(
				crate::datastore::FunctionId,
				crate::datastore::SchemaVersion,
			)>())
			.and_then(|bytes| {
				bytes.checked_add(entry.record_sources.len().checked_mul(std::mem::size_of::<(
					crate::values::Text,
					crate::datastore::ClassId,
				)>())?)
			})
			.and_then(|bytes| bytes.checked_add(std::mem::size_of::<LiveCatalogEntry>() + 512))
			.ok_or_else(|| invalid("LIVE query catalog metadata overflows"))?;
		live_budget.admit(bytes)?;
		for (name, _) in &entry.record_sources {
			live_budget.admit(name.len())?;
		}
	}
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
						maintenance: index.maintenance_mode(),
						build_revision: index.build_revision(),
						scope: index.scope(),
						fields: keys,
						options: index.options,
						physical: physical_indexes.get(&id).copied(),
						delta: physical_deltas.get(&id).copied(),
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
			let properties = schema
				.properties()
				.iter()
				.map(|(key, value)| {
					if value.is_none() {
						return Err(invalid("graph property cannot contain none"));
					}
					let value = StorageValue::from_runtime(value.clone(), context)?;
					Ok(GraphPropertyCatalogEntry {
						key,
						value: CanonicalCodec::encode(value.as_ref())?,
					})
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			let tensor_revision = root
				.graph(binding.graph_id)
				.ok_or_else(|| invalid("graph root is absent"))?
				.tensor_revision;
			let root = schema
				.root()
				.map(|root| {
					let value = StorageValue::from_runtime(Value::from(root.clone()), context)?;
					CanonicalCodec::encode(value.as_ref())
				})
				.transpose()?;
			Ok(GraphCatalogEntry {
				tensor_revision,
				id: binding.graph_id,
				version: schema.version(),
				published_at: schema.published_at,
				generation: binding.generation,
				name: schema.name().clone(),
				kind: schema.reference().kind(),
				storage_mode: GraphStorageMode::DiskBacked,
				topology_constraints: schema.topology_constraints(),
				label_index: (label.id(), label.version()),
				adjacency_index: (adjacency.id(), adjacency.version()),
				edge_endpoint_index: schema
					.edge_endpoint_index()
					.map(|index| (index.id(), index.version())),
				edge_endpoint: binding.edge_endpoint,
				indexes: schema.indexes().cloned().collect(),
				index_bindings: schema
					.indexes()
					.filter_map(|index| {
						let id = index.id()?;
						physical_indexes.get(&id).copied().map(|binding| (id, binding))
					})
					.collect(),
				membership: binding.membership,
				owner_root: schema.owner_root(),
				root,
				properties,
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
				comment: parameter.comment().cloned(),
			})
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let functions = root
		.schema
		.functions()
		.map(|function| FunctionCatalogEntry::from_definition(function))
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let analyzers = root
		.schema
		.analyzers()
		.map(|analyzer| {
			Ok(AnalyzerCatalogEntry {
				id: analyzer.id().ok_or_else(|| invalid("published analyzer has no AnalyzerId"))?,
				version: analyzer.version(),
				published_at: analyzer.published_at,
				name: analyzer.name().clone(),
				tokenizers: analyzer.tokenizers().collect(),
				filters: analyzer
					.filters()
					.map(|filter| encode_analyzer_filter(filter, context))
					.collect::<FerrosGrynnResult<Vec<_>>>()?,
				function: analyzer.function(),
				comment: analyzer.comment().cloned(),
			})
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;

	Ok(DatabaseCatalog {
		bridges: root.bridges.physical,
		event_outbox_segments: root.event_outbox_segments().as_ref().clone(),
		events: root.schema.events().map(|event| event.as_ref().clone()).collect(),
		database_uuid: root.database_uuid,
		committed_at_ns: root.committed_at_ns,
		live_history: root
			.live_catalog()
			.histories
			.iter()
			.map(|(id, metadata)| LiveHistoryCatalogEntry {
				id: *id,
				metadata: metadata.as_ref().clone(),
				segments: root
					.live_segments()
					.get(id)
					.map_or_else(Vec::new, |segments| segments.as_ref().clone()),
			})
			.collect(),
		live_bindings: root
			.live_catalog()
			.bindings
			.values()
			.map(|entry| {
				let diagnostic = match &entry.diagnostic {
					None => None,
					Some(error) => match error.status().status() {
						crate::gql_status::Status::LiveEvaluationFault {
							identity,
							origin_code,
							reason,
						} if identity == &entry.id.to_string() => {
							Some((origin_code.as_str().into(), reason.as_str().into()))
						}
						_ => {
							return Err(FerrosGrynnError::invalid_storage_file(
								"LIVE binding has a noncanonical fault diagnostic",
							));
						}
					},
				};
				Ok(LiveBindingCatalogEntry {
					id: entry.id,
					version: entry.version,
					state: entry.state,
					defined_at: entry.defined_at,
					published_at: entry.published_at,
					value_at: entry.value_at,
					options: entry.options.clone(),
					expression: entry.expression.clone(),
					parameters: entry.parameters.clone(),
					value: entry.value.clone(),
					descriptor: entry.descriptor.clone(),
					value_descriptor: entry.value_descriptor.clone(),
					dependencies: entry.dependencies.clone(),
					functions: entry.functions.clone(),
					record_sources: entry.record_sources.clone(),
					diagnostic,
				})
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?,
		live_queries: root
			.live_catalog()
			.queries
			.values()
			.map(|entry| {
				Ok(LiveCatalogEntry {
					graph_checkpoint: root.live_catalog().graph_segments.get(&entry.id).copied(),
					id: entry.id,
					version: entry.version,
					state: entry.state,
					defined_at: entry.defined_at,
					published_at: entry.published_at,
					acknowledged: entry.acknowledged,
					starting_after: entry.starting_after,
					options: entry.options.clone(),
					query: entry.query.clone(),
					descriptor: entry.descriptor.clone(),
					source_class: entry.source_class,
					capture_source: entry.capture_source,
					capture_scope_name: entry.capture_scope_name.clone(),
					rhs_scope_anchors: entry.rhs_scope_anchors.clone(),
					functions: entry.functions.clone(),
					record_sources: entry.record_sources.clone(),
					diagnostic: match &entry.diagnostic {
						None => None,
						Some(error) => match error.status().status() {
							crate::gql_status::Status::LiveEvaluationFault {
								identity,
								origin_code,
								reason,
							} if identity == &entry.id.to_string() => {
								Some((origin_code.as_str().into(), reason.as_str().into()))
							}
							_ => {
								return Err(invalid(
									"LIVE query diagnostic is not a restorable fault",
								));
							}
						},
					},
				})
			})
			.collect::<FerrosGrynnResult<_>>()?,
		changefeeds: root
			.changefeeds()
			.definitions
			.values()
			.map(|entry| ChangefeedCatalogEntry {
				id: entry.id,
				source: entry.source,
				name: entry.name.clone(),
				config: entry.config,
				state: entry.state,
				version: entry.version,
				defined_at: entry.defined_at,
				published_at: entry.published_at,
				last_stamp: entry.last_stamp,
				purged_through: entry.purged_through,
				purged_time_ns: entry.purged_time_ns,
				retained_page: entry.retained_page,
				retained_groups: entry.retained_groups,
				closed_at: entry.closed_at,
				closed_time_ns: entry.closed_time_ns,
				epochs: entry.epochs.clone(),
				segments: root
					.changefeed_segments()
					.get(&entry.id)
					.map_or_else(Vec::new, |segments| segments.as_ref().clone()),
			})
			.collect(),
		next_graph_id: root.schema.next_graph_id,
		next_schema_id: root.schema.next_schema_id.max(
			root.bridges.databases.keys().map(|id| u64::from(id.get()) + 1).max().unwrap_or(1),
		),
		time_zones: context.time_zone_names().cloned().collect(),
		atom_store,
		classes,
		graphs,
		parameters,
		functions,
		analyzers,
	})
}

/// Restores retained capture descriptors independently of surviving owners.
pub(super) fn decode_live(catalog: &DatabaseCatalog) -> crate::datastore::LiveCatalog {
	crate::datastore::LiveCatalog {
		graph_checkpoints: std::collections::BTreeMap::new(),
		graph_bytes: 0,
		graph_segments: decode_live_graph_segments(catalog),
		prepared_graphs: Arc::default(),
		prepared: Arc::default(),
		prepared_encoded: Arc::default(),
		histories: catalog
			.live_history
			.iter()
			.map(|entry| (entry.id, Arc::new(entry.metadata.clone())))
			.collect(),
		bindings: catalog
			.live_bindings
			.iter()
			.map(|entry| {
				(
					entry.id,
					Arc::new(crate::datastore::LiveBindingDefinition {
						id: entry.id,
						version: entry.version,
						state: entry.state,
						defined_at: entry.defined_at,
						published_at: entry.published_at,
						value_at: entry.value_at,
						options: entry.options.clone(),
						expression: entry.expression.clone(),
						parameters: entry.parameters.clone(),
						value: entry.value.clone(),
						descriptor: entry.descriptor.clone(),
						value_descriptor: entry.value_descriptor.clone(),
						dependencies: entry.dependencies.clone(),
						functions: entry.functions.clone(),
						record_sources: entry.record_sources.clone(),
						diagnostic: entry.diagnostic.as_ref().map(|(code, reason)| {
							FerrosGrynnError::live_evaluation_fault(
								entry.id.to_string(),
								code.as_str(),
								reason.as_str(),
							)
						}),
					}),
				)
			})
			.collect(),
		queries: catalog
			.live_queries
			.iter()
			.map(|entry| {
				(
					entry.id,
					Arc::new(crate::datastore::LiveDefinition {
						id: entry.id,
						version: entry.version,
						state: entry.state,
						defined_at: entry.defined_at,
						published_at: entry.published_at,
						acknowledged: entry.acknowledged,
						starting_after: entry.starting_after,
						options: entry.options.clone(),
						query: entry.query.clone(),
						descriptor: entry.descriptor.clone(),
						source_class: entry.source_class,
						capture_source: entry.capture_source,
						capture_scope_name: entry.capture_scope_name.clone(),
						rhs_scope_anchors: entry.rhs_scope_anchors.clone(),
						functions: entry.functions.clone(),
						record_sources: entry.record_sources.clone(),
						diagnostic: entry.diagnostic.as_ref().map(|(code, reason)| {
							FerrosGrynnError::live_evaluation_fault(
								entry.id.to_string(),
								code.as_str(),
								reason.as_str(),
							)
						}),
					}),
				)
			})
			.collect(),
	}
}

/// Restores compact query-owned graph checkpoint references.
pub(super) fn decode_live_graph_segments(
	catalog: &DatabaseCatalog,
) -> Arc<std::collections::BTreeMap<crate::live::LiveId, crate::storage::HistorySegment>> {
	Arc::new(
		catalog
			.live_queries
			.iter()
			.filter_map(|entry| entry.graph_checkpoint.map(|segment| (entry.id, segment)))
			.collect(),
	)
}

/// Restores retained capture descriptors independently of surviving owners.
pub(super) fn decode_changefeeds(catalog: &DatabaseCatalog) -> crate::datastore::ChangefeedCatalog {
	crate::datastore::ChangefeedCatalog {
		definitions: catalog
			.changefeeds
			.iter()
			.map(|entry| {
				(
					entry.id,
					Arc::new(crate::datastore::ChangefeedDefinition {
						id: entry.id,
						source: entry.source,
						name: entry.name.clone(),
						config: entry.config,
						state: entry.state,
						version: entry.version,
						defined_at: entry.defined_at,
						published_at: entry.published_at,
						last_stamp: entry.last_stamp,
						purged_through: entry.purged_through,
						purged_time_ns: entry.purged_time_ns,
						retained_page: entry.retained_page,
						retained_groups: entry.retained_groups,
						closed_at: entry.closed_at,
						closed_time_ns: entry.closed_time_ns,
						epochs: entry.epochs.clone(),
					}),
				)
			})
			.collect(),
	}
}

/// Shares catalog bounds with logical snapshots without decoding commit images.
pub(super) fn decode_changefeed_segments(
	catalog: &DatabaseCatalog,
) -> crate::storage::ChangefeedSegments {
	Arc::new(
		catalog
			.changefeeds
			.iter()
			.filter(|entry| !entry.segments.is_empty())
			.map(|entry| (entry.id, Arc::new(entry.segments.clone())))
			.collect(),
	)
}

/// Shares private computed-history ceilings without decoding historical values.
pub(super) fn decode_live_segments(
	catalog: &DatabaseCatalog,
) -> crate::storage::LiveHistorySegments {
	Arc::new(
		catalog
			.live_history
			.iter()
			.filter(|entry| !entry.segments.is_empty())
			.map(|entry| (entry.id, Arc::new(entry.segments.clone())))
			.collect(),
	)
}

fn encode_analyzer_filter(
	filter: &AnalyzerFilter,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<AnalyzerFilterCatalogEntry> {
	Ok(match filter {
		AnalyzerFilter::Nfc => AnalyzerFilterCatalogEntry::Nfc,
		AnalyzerFilter::Nfkc => AnalyzerFilterCatalogEntry::Nfkc,
		AnalyzerFilter::Ascii => AnalyzerFilterCatalogEntry::Ascii,
		AnalyzerFilter::Lowercase => AnalyzerFilterCatalogEntry::Lowercase,
		AnalyzerFilter::Uppercase => AnalyzerFilterCatalogEntry::Uppercase,
		AnalyzerFilter::EdgeNgram {
			min,
			max,
		} => AnalyzerFilterCatalogEntry::EdgeNgram {
			min: *min,
			max: *max,
		},
		AnalyzerFilter::Ngram {
			min,
			max,
		} => AnalyzerFilterCatalogEntry::Ngram {
			min: *min,
			max: *max,
		},
		AnalyzerFilter::Snowball(language) => AnalyzerFilterCatalogEntry::Snowball(*language),
		AnalyzerFilter::Mapper(entries) => {
			let mapper = Object::from_iter(entries.iter().map(|(source, target)| {
				(source.clone(), target.clone().map_or(Value::Null, Value::String))
			}));
			let value = StorageValue::from_runtime(Value::Object(mapper), context)?;
			AnalyzerFilterCatalogEntry::Mapper(CanonicalCodec::encode(value.as_ref())?)
		}
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
					maintenance: index.maintenance,
					build_revision: index.build_revision,
					scope: index.scope,
					fields: names,
					options: index.options,
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
		let mut graph = crate::datastore::GraphSchema::new(
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
			crate::values::PropertyMap::new(),
			entry.topology_constraints,
			None,
			entry.owner_root,
			entry.published_at,
		);
		graph.indexes = entry.indexes.clone();
		if let Some((id, version)) = entry.edge_endpoint_index {
			graph = graph.with_edge_endpoint_index(SystemIndex::from_storage(
				id,
				version,
				SystemIndexKind::EdgeEndpoint,
			))?;
		}
		let graph = Arc::new(graph);
		if entry.owner_root.is_none() {
			schema.graph_names.insert(entry.name.to_string(), entry.id);
		}
		schema.graphs.insert(entry.id, graph);
		schema.graph_versions.insert(entry.id, entry.published_at);
	}
	for entry in &catalog.functions {
		let function = entry.clone().into_definition()?;
		let signature = function.signature();
		schema.function_signatures.insert(signature, entry.id);
		schema.functions.insert(entry.id, Arc::new(function));
		schema.function_versions.insert(entry.id, entry.published_at);
	}
	for event in &catalog.events {
		let id = event.id().ok_or_else(|| invalid("published EVENT has no identity"))?;
		schema.event_names.insert((event.target(), event.name().clone()), id);
		schema.events.insert(id, Arc::new(event.clone()));
	}
	Ok(schema)
}

pub(super) fn decode_catalog_values(
	catalog: &DatabaseCatalog,
	dictionary: &PropertyDictionary,
	schema: &mut SchemaCatalog,
) -> FerrosGrynnResult<()> {
	let mut context = value_context(schema, dictionary.clone())?;
	for zone in &catalog.time_zones {
		context.register_time_zone_name(zone.clone())?;
	}
	for entry in &catalog.graphs {
		let mut properties = crate::values::PropertyMap::with_capacity(entry.properties.len());
		for property in &entry.properties {
			if dictionary.resolve(property.key).is_none() {
				return Err(invalid("graph property references an unknown property key"));
			}
			let value = CanonicalCodec::decode(&property.value, DecodeLimits::default())?
				.into_runtime(&context.decoder())?;
			if value.is_none() || properties.insert(property.key, value).is_some() {
				return Err(invalid("graph property value or key is invalid"));
			}
		}
		let current = schema
			.graph(entry.id)
			.ok_or_else(|| invalid("graph property entry has no graph definition"))?;
		let root = entry
			.root
			.as_ref()
			.map(|bytes| {
				let value = CanonicalCodec::decode(bytes, DecodeLimits::default())?
					.into_runtime(&context.decoder())?;
				match value {
					Value::Record(record) => Ok(*record),
					_ => Err(invalid("graph root is not a canonical RecordId")),
				}
			})
			.transpose()?;
		let mut graph = crate::datastore::GraphSchema::new(
			entry.version,
			current.reference().clone(),
			current
				.system_indexes()
				.find(|index| index.kind() == SystemIndexKind::Label)
				.copied()
				.ok_or_else(|| invalid("graph label index is absent"))?,
			current
				.system_indexes()
				.find(|index| index.kind() == SystemIndexKind::Adjacency)
				.copied()
				.ok_or_else(|| invalid("graph adjacency index is absent"))?,
			properties,
			entry.topology_constraints,
			root,
			entry.owner_root,
			entry.published_at,
		);
		graph.indexes = entry.indexes.clone();
		if let Some((id, version)) = entry.edge_endpoint_index {
			graph = graph.with_edge_endpoint_index(SystemIndex::from_storage(
				id,
				version,
				SystemIndexKind::EdgeEndpoint,
			))?;
		}
		let graph = Arc::new(graph);
		schema.graphs.insert(entry.id, graph);
	}
	for entry in &catalog.parameters {
		let value = CanonicalCodec::decode(&entry.value, DecodeLimits::default())?
			.into_runtime(&context.decoder())?;
		let parameter = Arc::new(SchemaParameter {
			id: Some(entry.id),
			version: entry.version,
			name: entry.name.clone(),
			value,
			comment: entry.comment.clone(),
			published_at: entry.published_at,
		});
		schema.parameters.insert(entry.name.to_string(), parameter);
		schema.parameter_versions.insert(entry.name.to_string(), entry.published_at);
	}
	for entry in &catalog.analyzers {
		let filters = entry
			.filters
			.iter()
			.map(|filter| decode_analyzer_filter(filter, &context.decoder()))
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let mut analyzer =
			SchemaAnalyzer::new(entry.name.clone(), entry.tokenizers.iter().copied(), filters)?;
		if let Some((id, version)) = entry.function {
			analyzer = analyzer.with_function(id, version)?;
		}
		analyzer.id = Some(entry.id);
		analyzer.version = entry.version;
		analyzer.published_at = entry.published_at;
		analyzer.comment = entry.comment.clone();
		schema.analyzer_names.insert(entry.name.to_string(), entry.id);
		schema.analyzers.insert(entry.id, Arc::new(analyzer));
		schema.analyzer_versions.insert(entry.id, entry.published_at);
	}
	Ok(())
}

fn decode_analyzer_filter(
	filter: &AnalyzerFilterCatalogEntry,
	context: &crate::values::storage::DecodeContext<'_>,
) -> FerrosGrynnResult<AnalyzerFilter> {
	Ok(match filter {
		AnalyzerFilterCatalogEntry::Nfc => AnalyzerFilter::Nfc,
		AnalyzerFilterCatalogEntry::Nfkc => AnalyzerFilter::Nfkc,
		AnalyzerFilterCatalogEntry::Ascii => AnalyzerFilter::Ascii,
		AnalyzerFilterCatalogEntry::Lowercase => AnalyzerFilter::Lowercase,
		AnalyzerFilterCatalogEntry::Uppercase => AnalyzerFilter::Uppercase,
		AnalyzerFilterCatalogEntry::EdgeNgram {
			min,
			max,
		} => AnalyzerFilter::EdgeNgram {
			min: *min,
			max: *max,
		},
		AnalyzerFilterCatalogEntry::Ngram {
			min,
			max,
		} => AnalyzerFilter::Ngram {
			min: *min,
			max: *max,
		},
		AnalyzerFilterCatalogEntry::Snowball(language) => AnalyzerFilter::Snowball(*language),
		AnalyzerFilterCatalogEntry::Mapper(bytes) => {
			let value =
				CanonicalCodec::decode(bytes, DecodeLimits::default())?.into_runtime(context)?;
			let Value::Object(entries) = value else {
				return Err(invalid("analyzer mapper is not an object"));
			};
			let mut mapper = BTreeMap::new();
			for (source, target) in entries {
				let target = match target {
					Value::String(value) => Some(value),
					Value::Null => None,
					_ => return Err(invalid("analyzer mapper value is neither string nor null")),
				};
				mapper.insert(source, target);
			}
			AnalyzerFilter::Mapper(mapper)
		}
	})
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

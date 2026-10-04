use std::{
	collections::{BTreeMap, BTreeSet},
	num::NonZeroU16,
};

use super::*;
use crate::{
	datastore::{
		AnalyzerId,
		AnalyzerTokenizer,
		ClassIndexKind,
		ClassIndexOptions,
		ClassIndexState,
		ClassSchemaMode,
		DiskAnnIndexOptions,
		FulltextIndexOptions,
		FunctionEffect,
		FunctionId,
		HnswIndexOptions,
		IndexScope,
		VectorDistance,
	},
	gql_status::FerrosGrynnResult,
	graph::{AtomId, AtomKind, GraphKind, GraphTopologyConstraints},
	storage::types::{DataFileId, DataFileKind},
	values::{
		PropertyKeyId,
		Type,
		VectorKind,
		storage::{StorageValue, codec::CanonicalCodec},
	},
};

#[test]
fn fgdc_v10_round_trips_recursive_types_bindings_and_graph_properties() -> FerrosGrynnResult<()> {
	let catalog = fixture()?;
	let bytes = catalog.encode()?;
	assert_eq!(&bytes[..4], b"FGDC");
	assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 10);
	assert_eq!(DatabaseCatalog::decode(&bytes)?, catalog);
	Ok(())
}

#[test]
fn fgdc_changefeed_origin_bounds_and_reserved_bytes_are_checked() -> FerrosGrynnResult<()> {
	use crate::changefeed::{ChangefeedConfig, ChangefeedId, ChangefeedSource, ChangefeedState};
	let mut catalog = fixture()?;
	catalog.changefeeds.push(ChangefeedCatalogEntry {
		id: ChangefeedId::from_uuid(uuid::Uuid::from_u128(42).into())?,
		source: ChangefeedSource::Class(catalog.classes[0].id),
		name: "person".into(),
		config: ChangefeedConfig::from_nanoseconds(1_234)?
			.with_full(true)
			.with_include_original(true),
		state: ChangefeedState::Active,
		version: SchemaVersion::INITIAL,
		defined_at: 1,
		published_at: 2,
		last_stamp: u64::MAX,
		purged_through: 9,
		purged_time_ns: 0,
		retained_page: 0,
		retained_groups: 0,
		closed_at: None,
		closed_time_ns: None,
		segments: Vec::new(),
		epochs: vec![crate::changefeed::ChangefeedCaptureEpoch {
			started_at: 2.into(),
			configuration: Some(
				ChangefeedConfig::from_nanoseconds(1_234)?
					.with_full(true)
					.with_include_original(true),
			),
		}],
	});
	let bytes = catalog.encode()?;
	assert_eq!(&bytes[88..104], catalog.database_uuid.as_bytes());
	assert_eq!(&bytes[104..120], &catalog.committed_at_ns.to_le_bytes());
	assert_eq!(&bytes[120..124], &1_u32.to_le_bytes());
	assert_eq!(&bytes[124..128], &[0; 4]);
	assert_eq!(DatabaseCatalog::decode(&bytes)?, catalog);
	let descriptor = bytes.len() - 152 - 32 - "person".len();
	assert_eq!(&bytes[descriptor..descriptor + 16], uuid::Uuid::from_u128(42).as_bytes());
	assert_eq!(&bytes[descriptor + 16..descriptor + 20], &[0, 0, 3, 0]);
	for offset in
		[124, descriptor + 16, descriptor + 17, descriptor + 18, descriptor + 19, descriptor + 32]
	{
		let mut malformed = bytes.clone();
		malformed[offset] = u8::MAX;
		assert!(DatabaseCatalog::decode(&malformed).is_err(), "offset {offset}");
	}
	for end in descriptor..bytes.len() {
		assert!(DatabaseCatalog::decode(&bytes[..end]).is_err());
	}
	let mut huge_count = bytes.clone();
	huge_count[120..124].copy_from_slice(&u32::MAX.to_le_bytes());
	assert!(DatabaseCatalog::decode(&huge_count).is_err());
	let mut nil_origin = bytes.clone();
	nil_origin[88..104].fill(0);
	assert!(DatabaseCatalog::decode(&nil_origin).is_err());
	catalog.changefeeds[0].state = ChangefeedState::Retired;
	catalog.changefeeds[0].closed_at = Some(2);
	catalog.changefeeds[0].closed_time_ns = Some(catalog.committed_at_ns);
	catalog.changefeeds[0].epochs[0].configuration = None;
	catalog.classes.clear();
	assert_eq!(DatabaseCatalog::decode(&catalog.encode()?)?, catalog);
	for source in [
		ChangefeedSource::Class(ClassId::from_storage(catalog.next_schema_id as u32)?),
		ChangefeedSource::Graph(
			GraphId::new(catalog.next_graph_id)
				.ok_or_else(|| codec::invalid("fixture graph clock is zero"))?,
		),
	] {
		let mut future = catalog.clone();
		future.changefeeds[0].source = source;
		assert!(future.encode().is_err());
	}
	catalog.changefeeds[0].name = "invalid\0name".into();
	assert!(catalog.encode().is_err());
	Ok(())
}

/// Trusted segment bounds remain decodable independently of file contents.
#[test]
fn fgdc_changefeed_segment_descriptors_check_ids_bounds_lengths_and_reserved()
-> FerrosGrynnResult<()> {
	use crate::{
		changefeed::{ChangefeedConfig, ChangefeedId, ChangefeedSource, ChangefeedState},
		storage::{ChangefeedSegment, changefeed::SegmentHeader},
	};
	let mut catalog = fixture()?;
	let id = ChangefeedId::from_uuid(uuid::Uuid::from_u128(42).into())?;
	let config = ChangefeedConfig::from_nanoseconds(1_234)?;
	let segment = ChangefeedSegment {
		file: DataFileId::new(99, DataFileKind::Changefeed)
			.ok_or_else(|| codec::invalid("fixture file identity invalid"))?,
		header: SegmentHeader {
			database: catalog.database_uuid,
			feed: id,
			segment: uuid::Uuid::from_u128(51),
			pages: 2,
			commits: 1,
			first_stamp: 10,
			last_stamp: 10,
			first_time: 0,
			last_time: 0,
		},
	};
	catalog.changefeeds.push(ChangefeedCatalogEntry {
		id,
		source: ChangefeedSource::Class(catalog.classes[0].id),
		name: "person".into(),
		config,
		state: ChangefeedState::Active,
		version: SchemaVersion::INITIAL,
		defined_at: 1,
		published_at: 1,
		last_stamp: 10,
		purged_through: 0,
		purged_time_ns: 0,
		retained_page: 0,
		retained_groups: 0,
		closed_at: None,
		closed_time_ns: None,
		epochs: vec![crate::changefeed::ChangefeedCaptureEpoch {
			started_at: 1.into(),
			configuration: Some(config),
		}],
		segments: vec![segment],
	});
	let bytes = catalog.encode()?;
	assert_eq!(DatabaseCatalog::decode(&bytes)?, catalog);
	let start = bytes.len() - 128;
	assert_eq!(&bytes[start..start + 8], &segment.file.get().to_le_bytes());
	assert_eq!(&bytes[start + 56..start + 64], &2_u64.to_le_bytes());
	assert_eq!(&bytes[start + 120..], &[0; 8]);
	for offset in [start + 120, start + 127] {
		let mut malformed = bytes.clone();
		malformed[offset] = u8::MAX;
		assert!(DatabaseCatalog::decode(&malformed).is_err(), "offset {offset}");
	}
	for (offset, value) in [(start + 56, 1_u64), (start + 64, 0_u64), (start + 64, 2_u64)] {
		let mut malformed = bytes.clone();
		malformed[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
		assert!(DatabaseCatalog::decode(&malformed).is_err(), "offset {offset}, value {value}");
	}
	let mut count = bytes.clone();
	count[start - 4..start].copy_from_slice(&u32::MAX.to_le_bytes());
	assert!(DatabaseCatalog::decode(&count).is_err());
	for end in start..bytes.len() {
		assert!(DatabaseCatalog::decode(&bytes[..end]).is_err());
	}
	let mut bad = catalog.clone();
	bad.changefeeds[0].segments[0].header.feed =
		ChangefeedId::from_uuid(uuid::Uuid::from_u128(43).into())?;
	assert!(bad.encode().is_err());
	let mut bad = catalog.clone();
	bad.changefeeds[0].segments[0].header.last_stamp = 11;
	assert!(bad.encode().is_err());
	let mut bad = catalog.clone();
	bad.changefeeds[0].segments.push(segment);
	assert!(bad.encode().is_err());
	Ok(())
}

#[test]
fn fgdc_v10_round_trips_fulltext_hnsw_and_disk_ann_payloads() -> FerrosGrynnResult<()> {
	let mut catalog = fixture()?;
	let class = &mut catalog.classes[0];
	class.fields.extend([
		ClassFieldCatalogEntry {
			id: SchemaId::new(9).ok_or_else(|| codec::invalid("invalid title field"))?,
			version: SchemaVersion::INITIAL,
			name: "title".into(),
			value_type: Type::String,
			property_key: PropertyKeyId::new(1),
		},
		ClassFieldCatalogEntry {
			id: SchemaId::new(10).ok_or_else(|| codec::invalid("invalid vector field"))?,
			version: SchemaVersion::INITIAL,
			name: "embedding".into(),
			value_type: Type::Vector(VectorKind::F32),
			property_key: PropertyKeyId::new(2),
		},
		ClassFieldCatalogEntry {
			id: SchemaId::new(11).ok_or_else(|| codec::invalid("invalid compact field"))?,
			version: SchemaVersion::INITIAL,
			name: "compact".into(),
			value_type: Type::Vector(VectorKind::F16),
			property_key: PropertyKeyId::new(3),
		},
	]);
	let binding = |instance| -> FerrosGrynnResult<PhysicalIndexBinding> {
		Ok(PhysicalIndexBinding {
			file_id: DataFileId::new(instance, DataFileKind::Index)
				.ok_or_else(|| codec::invalid("invalid specialized fixture file"))?,
			root: crate::storage::page::PageNo::new(0),
		})
	};
	class.indexes.extend([
		ClassIndexCatalogEntry {
			id: IndexId::new(12).ok_or_else(|| codec::invalid("invalid Fulltext index"))?,
			version: SchemaVersion::INITIAL,
			name: "title_search".into(),
			kind: ClassIndexKind::Fulltext,
			state: ClassIndexState::Ready,
			maintenance: crate::datastore::IndexMaintenanceMode::Immediate,
			build_revision: 3,
			scope: IndexScope::Base,
			fields: vec![PropertyKeyId::new(1)],
			options: ClassIndexOptions::Fulltext(FulltextIndexOptions::new(
				AnalyzerId::new(8).ok_or_else(|| codec::invalid("invalid analyzer"))?,
				SchemaVersion::INITIAL,
				1.4,
				0.6,
				true,
			)?),
			physical: Some(binding(6)?),
			delta: Some(binding(7)?),
		},
		ClassIndexCatalogEntry {
			id: IndexId::new(13).ok_or_else(|| codec::invalid("invalid HNSW index"))?,
			version: SchemaVersion::INITIAL,
			name: "embedding_hnsw".into(),
			kind: ClassIndexKind::Hnsw,
			state: ClassIndexState::Ready,
			maintenance: crate::datastore::IndexMaintenanceMode::Immediate,
			build_revision: 3,
			scope: IndexScope::Base,
			fields: vec![PropertyKeyId::new(2)],
			options: ClassIndexOptions::Hnsw(HnswIndexOptions::new(
				VectorKind::F32,
				NonZeroU16::new(128).ok_or_else(|| codec::invalid("invalid dimension"))?,
				VectorDistance::Cosine,
				NonZeroU16::new(16).ok_or_else(|| codec::invalid("invalid M"))?,
				NonZeroU16::new(180).ok_or_else(|| codec::invalid("invalid EFC"))?,
			)?),
			physical: Some(binding(8)?),
			delta: Some(binding(9)?),
		},
		ClassIndexCatalogEntry {
			id: IndexId::new(14).ok_or_else(|| codec::invalid("invalid DiskANN index"))?,
			version: SchemaVersion::INITIAL,
			name: "compact_disk".into(),
			kind: ClassIndexKind::DiskAnn,
			state: ClassIndexState::Ready,
			maintenance: crate::datastore::IndexMaintenanceMode::Deferred,
			build_revision: 3,
			scope: IndexScope::Base,
			fields: vec![PropertyKeyId::new(3)],
			options: ClassIndexOptions::DiskAnn(DiskAnnIndexOptions::new(
				VectorKind::F16,
				NonZeroU16::new(768).ok_or_else(|| codec::invalid("invalid dimension"))?,
				VectorDistance::CosineNormalized,
				NonZeroU16::new(48).ok_or_else(|| codec::invalid("invalid degree"))?,
				NonZeroU16::new(120).ok_or_else(|| codec::invalid("invalid build list"))?,
				1.3,
				true,
			)?),
			physical: Some(binding(10)?),
			delta: Some(binding(11)?),
		},
	]);
	catalog.next_schema_id = 15;
	let decoded = DatabaseCatalog::decode(&catalog.encode()?)?;
	assert_eq!(decoded, catalog);
	Ok(())
}

#[test]
fn fgdc_rejects_old_version_truncation_and_duplicate_schema_identity() -> FerrosGrynnResult<()> {
	let catalog = fixture()?;
	let mut old = catalog.encode()?;
	old[4..6].copy_from_slice(&9_u16.to_le_bytes());
	assert!(DatabaseCatalog::decode(&old).is_err());

	let mut truncated = catalog.encode()?;
	truncated.pop();
	assert!(DatabaseCatalog::decode(&truncated).is_err());

	let mut duplicate = catalog;
	duplicate.parameters[0].id = duplicate.classes[0].fields[0].id;
	assert!(duplicate.encode().is_err());
	Ok(())
}

#[test]
fn fgdc_rejects_duplicate_or_unsorted_graph_properties() -> FerrosGrynnResult<()> {
	let mut catalog = fixture()?;
	catalog.graphs[0].properties.push(GraphPropertyCatalogEntry {
		key: PropertyKeyId::new(0),
		value: vec![1],
	});
	assert!(catalog.encode().is_err());
	Ok(())
}

#[test]
fn fgdc_v10_allows_one_internal_child_through_multiple_owner_scopes() -> FerrosGrynnResult<()> {
	let catalog = fixture_with_shared_child()?;
	let decoded = DatabaseCatalog::decode(&catalog.encode()?)?;
	let root = &decoded.graphs[0];
	assert_eq!(root.arena.graph_by_slot[&1], root.arena.graph_by_slot[&2]);
	assert_ne!(root.arena.scope_by_slot[&1], root.arena.scope_by_slot[&2]);
	assert_eq!(decoded.graphs[1].owner_root, Some(root.id));
	Ok(())
}

#[test]
fn fgdc_v10_rejects_duplicate_scope_and_noncanonical_internal_name() -> FerrosGrynnResult<()> {
	let mut duplicate_scope = fixture_with_shared_child()?;
	let first_scope = duplicate_scope.graphs[0].arena.scope_by_slot[&1].clone();
	duplicate_scope.graphs[0].arena.scope_by_slot.insert(2, first_scope);
	assert!(duplicate_scope.encode().is_err());

	let mut invalid_name = fixture_with_shared_child()?;
	invalid_name.graphs[1].name = "$fg$build_wrong".into();
	assert!(invalid_name.encode().is_err());
	Ok(())
}

#[test]
fn fgdc_v10_rejects_unknown_constraint_bits_and_invalid_roots() -> FerrosGrynnResult<()> {
	let catalog = fixture()?;
	let mut unknown_bits = catalog.encode()?;
	let graph_header = first_graph_header(&unknown_bits, &catalog.graphs[0])?;
	unknown_bits[graph_header + 30..graph_header + 32].copy_from_slice(&u16::MAX.to_le_bytes());
	assert!(DatabaseCatalog::decode(&unknown_bits).is_err());

	let mut wrong_family = fixture()?;
	wrong_family.graphs[0].root = Some(vec![1]);
	assert!(wrong_family.encode().is_err());

	let mut missing_root = fixture()?;
	missing_root.graphs[0].kind = GraphKind::RootedTree;
	assert!(missing_root.encode().is_ok());
	Ok(())
}

#[test]
fn fgdc_v10_rejects_invalid_internal_ownership_and_scope_ancestry() -> FerrosGrynnResult<()> {
	let mut missing_owner = fixture_with_shared_child()?;
	missing_owner.graphs[1].owner_root = GraphId::new(99);
	assert!(missing_owner.encode().is_err());

	let mut orphan = fixture_with_shared_child()?;
	orphan.graphs[0].arena.graph_by_slot.remove(&1);
	orphan.graphs[0].arena.scope_by_slot.remove(&1);
	orphan.graphs[0].arena.graph_by_slot.remove(&2);
	orphan.graphs[0].arena.scope_by_slot.remove(&2);
	assert!(orphan.encode().is_err());

	let mut missing_parent = fixture_with_shared_child()?;
	let owner = missing_parent.graphs[0].arena.scope_by_slot[&1][0];
	let nested = AtomId::from_parts(AtomKind::Node, 2, 0)?;
	missing_parent.graphs[0].arena.scope_by_slot.insert(2, vec![owner, nested].into_boxed_slice());
	assert!(missing_parent.encode().is_err());

	let mut cycle = fixture_with_shared_child()?;
	let first = cycle.graphs[0].arena.scope_by_slot[&1][0];
	let second = cycle.graphs[0].arena.scope_by_slot[&2][0];
	cycle.graphs[0].arena.scope_by_slot.insert(2, vec![first, second].into_boxed_slice());
	assert!(cycle.encode().is_err());
	Ok(())
}

fn first_graph_header(bytes: &[u8], graph: &GraphCatalogEntry) -> FerrosGrynnResult<usize> {
	let mut prefix = Vec::new();
	prefix.extend_from_slice(&graph.id.get().to_le_bytes());
	prefix.extend_from_slice(&graph.version.get().to_le_bytes());
	prefix.extend_from_slice(&graph.published_at.to_le_bytes());
	prefix.extend_from_slice(&graph.generation.to_le_bytes());
	bytes
		.windows(prefix.len())
		.position(|window| window == prefix)
		.ok_or_else(|| codec::invalid("fixture graph header is absent"))
}

fn fixture() -> FerrosGrynnResult<DatabaseCatalog> {
	let graph = GraphId::new(7).ok_or_else(|| codec::invalid("invalid fixture graph"))?;
	let index = |instance| -> FerrosGrynnResult<PhysicalIndexBinding> {
		Ok(PhysicalIndexBinding {
			file_id: DataFileId::new(instance, DataFileKind::Index)
				.ok_or_else(|| codec::invalid("invalid fixture file"))?,
			root: crate::storage::page::PageNo::new(0),
		})
	};
	Ok(DatabaseCatalog {
		database_uuid: uuid::Uuid::from_u128(1),
		committed_at_ns: 123_456_789,
		changefeeds: Vec::new(),
		next_graph_id: 8,
		next_schema_id: 9,
		time_zones: vec!["Europe/London".into()],
		atom_store: AtomStoreCatalogBinding {
			generation: 11,
			primary: index(1)?,
			reverse_membership: index(2)?,
		},
		classes: vec![ClassCatalogEntry {
			id: ClassId::new(1).ok_or_else(|| codec::invalid("invalid fixture class"))?,
			version: SchemaVersion::INITIAL,
			published_at: 1,
			name: "person".into(),
			kind: AtomKind::Node,
			mode: ClassSchemaMode::Schemafull,
			fields: vec![ClassFieldCatalogEntry {
				id: SchemaId::new(2).ok_or_else(|| codec::invalid("invalid fixture field"))?,
				version: SchemaVersion::INITIAL,
				name: "home".into(),
				value_type: Type::Option(Box::new(Type::GraphRef(GraphKind::Graph))),
				property_key: PropertyKeyId::new(0),
			}],
			indexes: vec![ClassIndexCatalogEntry {
				id: IndexId::new(3).ok_or_else(|| codec::invalid("invalid fixture index"))?,
				version: SchemaVersion::INITIAL,
				name: "by_home".into(),
				kind: ClassIndexKind::Unique,
				state: ClassIndexState::Ready,
				maintenance: crate::datastore::IndexMaintenanceMode::Immediate,
				build_revision: 1,
				scope: IndexScope::Base,
				fields: vec![PropertyKeyId::new(0)],
				options: crate::datastore::ClassIndexOptions::Scalar,
				physical: Some(index(3)?),
				delta: None,
			}],
		}],
		graphs: vec![GraphCatalogEntry {
			tensor_revision: 0,
			id: graph,
			version: SchemaVersion::INITIAL,
			published_at: 1,
			generation: 11,
			name: "social".into(),
			kind: GraphKind::Graph,
			storage_mode: GraphStorageMode::DiskBacked,
			topology_constraints: GraphTopologyConstraints::empty(),
			label_index: (
				IndexId::new(4).ok_or_else(|| codec::invalid("invalid label index"))?,
				SchemaVersion::INITIAL,
			),
			adjacency_index: (
				IndexId::new(5).ok_or_else(|| codec::invalid("invalid adjacency index"))?,
				SchemaVersion::INITIAL,
			),
			edge_endpoint_index: None,
			edge_endpoint: None,
			membership: index(4)?,
			indexes: Vec::new(),
			index_bindings: BTreeMap::new(),
			owner_root: None,
			root: None,
			properties: vec![GraphPropertyCatalogEntry {
				key: PropertyKeyId::new(0),
				value: vec![1],
			}],
			arena: GraphArenaBindings {
				graph_by_slot: BTreeMap::from([(0, graph)]),
				scope_by_slot: BTreeMap::from([(0, Box::default())]),
			},
		}],
		parameters: vec![ParameterCatalogEntry {
			id: SchemaId::new(6).ok_or_else(|| codec::invalid("invalid fixture parameter"))?,
			version: SchemaVersion::INITIAL,
			published_at: 1,
			name: "answer".into(),
			value: vec![0],
			comment: Some("durable parameter".into()),
		}],
		functions: vec![FunctionCatalogEntry {
			id: FunctionId::new(7).ok_or_else(|| codec::invalid("invalid fixture function"))?,
			version: SchemaVersion::INITIAL,
			published_at: 1,
			namespace: "fn".into(),
			name: "increment".into(),
			parameters: vec![FunctionParameterCatalogEntry {
				name: "value".into(),
				value_type: Type::Int,
			}],
			result: Type::Int,
			effect: FunctionEffect::Pure,
			cost: false,
			max_depth: None,
			body: "{ RETURN $value + 1; }".into(),
			comment: Some("fixture function".into()),
		}],
		analyzers: vec![AnalyzerCatalogEntry {
			id: AnalyzerId::new(8).ok_or_else(|| codec::invalid("invalid fixture analyzer"))?,
			version: SchemaVersion::INITIAL,
			published_at: 1,
			name: "english".into(),
			tokenizers: BTreeSet::from([AnalyzerTokenizer::Blank, AnalyzerTokenizer::Punct]),
			filters: vec![
				AnalyzerFilterCatalogEntry::Lowercase,
				AnalyzerFilterCatalogEntry::Mapper(CanonicalCodec::encode(
					StorageValue::Object(vec![(
						PropertyKeyId::new(0),
						StorageValue::String("drive".into()),
					)])
					.as_ref(),
				)?),
			],
			function: None,
			comment: Some("fixture analyzer".into()),
		}],
	})
}

#[test]
fn fgdc_tensor_bindings_require_the_dedicated_family_without_weakening_indexes()
-> FerrosGrynnResult<()> {
	use crate::{
		datastore::{GraphIndex, TensorCacheFormat},
		graph::tensor::TensorScope,
	};
	let mut catalog = fixture()?;
	let graph = &mut catalog.graphs[0];
	let mut definition =
		GraphIndex::new_tensor("tensor", TensorScope::Local, TensorCacheFormat::Csr, 1 << 20)?;
	let id = IndexId::new(9).ok_or_else(|| codec::invalid("invalid fixture index"))?;
	definition.id = Some(id);
	definition.graph = Some(graph.id);
	definition.state = ClassIndexState::Ready;
	definition.build_revision = 11;
	graph.indexes.push(definition);
	let binding = PhysicalIndexBinding {
		file_id: DataFileId::new(20, DataFileKind::TensorCache)
			.ok_or_else(|| codec::invalid("invalid tensor file"))?,
		root: crate::storage::page::PageNo::new(0),
	};
	graph.index_bindings.insert(id, binding);
	catalog.next_schema_id = 10;
	let bytes = catalog.encode()?;
	let decoded = DatabaseCatalog::decode(&bytes)?;
	assert_eq!(decoded.graphs[0].index_bindings[&id], binding);
	catalog.graphs[0]
		.index_bindings
		.get_mut(&id)
		.ok_or_else(|| codec::invalid("missing tensor binding"))?
		.file_id = DataFileId::new(20, DataFileKind::Index)
		.ok_or_else(|| codec::invalid("invalid ordinary file"))?;
	assert!(catalog.encode().is_err());
	catalog.graphs[0].index_bindings.insert(id, binding);
	catalog.atom_store.primary = binding;
	assert!(catalog.encode().is_err());
	Ok(())
}

fn fixture_with_shared_child() -> FerrosGrynnResult<DatabaseCatalog> {
	let mut catalog = fixture()?;
	let root = catalog.graphs[0].id;
	let child = GraphId::new(8).ok_or_else(|| codec::invalid("invalid child graph"))?;
	let owner_a = AtomId::from_parts(AtomKind::Node, 0, 0)?;
	let owner_b = AtomId::from_parts(AtomKind::Node, 1, 0)?;
	catalog.next_graph_id = 9;
	catalog.next_schema_id = 11;
	catalog.graphs[0].arena.graph_by_slot.extend([(1, child), (2, child)]);
	catalog.graphs[0]
		.arena
		.scope_by_slot
		.extend([(1, vec![owner_a].into_boxed_slice()), (2, vec![owner_b].into_boxed_slice())]);
	catalog.graphs.push(GraphCatalogEntry {
		tensor_revision: 0,
		id: child,
		version: SchemaVersion::INITIAL,
		published_at: 1,
		generation: 12,
		name: format!("$fg$build_{:016x}_{:016x}", root.get(), child.get()).into(),
		kind: GraphKind::Graph,
		storage_mode: GraphStorageMode::DiskBacked,
		topology_constraints: GraphTopologyConstraints::empty(),
		label_index: (
			IndexId::new(9).ok_or_else(|| codec::invalid("invalid label index"))?,
			SchemaVersion::INITIAL,
		),
		adjacency_index: (
			IndexId::new(10).ok_or_else(|| codec::invalid("invalid adjacency index"))?,
			SchemaVersion::INITIAL,
		),
		edge_endpoint_index: None,
		edge_endpoint: None,
		membership: PhysicalIndexBinding {
			file_id: DataFileId::new(5, DataFileKind::Index)
				.ok_or_else(|| codec::invalid("invalid child membership"))?,
			root: crate::storage::page::PageNo::new(0),
		},
		owner_root: Some(root),
		indexes: Vec::new(),
		index_bindings: BTreeMap::new(),
		root: None,
		properties: Vec::new(),
		arena: GraphArenaBindings {
			graph_by_slot: BTreeMap::from([(0, child)]),
			scope_by_slot: BTreeMap::from([(0, Box::default())]),
		},
	});
	Ok(catalog)
}

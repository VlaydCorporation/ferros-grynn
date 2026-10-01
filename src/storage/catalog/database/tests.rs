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
			membership: index(4)?,
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
		membership: PhysicalIndexBinding {
			file_id: DataFileId::new(5, DataFileKind::Index)
				.ok_or_else(|| codec::invalid("invalid child membership"))?,
			root: crate::storage::page::PageNo::new(0),
		},
		owner_root: Some(root),
		root: None,
		properties: Vec::new(),
		arena: GraphArenaBindings {
			graph_by_slot: BTreeMap::from([(0, child)]),
			scope_by_slot: BTreeMap::from([(0, Box::default())]),
		},
	});
	Ok(catalog)
}

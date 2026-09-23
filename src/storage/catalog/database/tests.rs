use std::collections::BTreeMap;

use super::*;
use crate::{
	datastore::{ClassIndexKind, ClassIndexState, ClassSchemaMode, IndexScope},
	gql_status::FerrosGrynnResult,
	graph::{AtomKind, GraphKind},
	storage::types::{DataFileId, DataFileKind},
	values::{PropertyKeyId, Type},
};

#[test]
fn fgdc_v7_round_trips_recursive_types_and_bindings() -> FerrosGrynnResult<()> {
	let catalog = fixture()?;
	let bytes = catalog.encode()?;
	assert_eq!(&bytes[..4], b"FGDC");
	assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 7);
	assert_eq!(DatabaseCatalog::decode(&bytes)?, catalog);
	Ok(())
}

#[test]
fn fgdc_rejects_old_version_truncation_and_duplicate_schema_identity() -> FerrosGrynnResult<()> {
	let catalog = fixture()?;
	let mut old = catalog.encode()?;
	old[4..6].copy_from_slice(&6_u16.to_le_bytes());
	assert!(DatabaseCatalog::decode(&old).is_err());

	let mut truncated = catalog.encode()?;
	truncated.pop();
	assert!(DatabaseCatalog::decode(&truncated).is_err());

	let mut duplicate = catalog;
	duplicate.parameters[0].id = duplicate.classes[0].fields[0].id;
	assert!(duplicate.encode().is_err());
	Ok(())
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
		next_schema_id: 7,
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
				scope: IndexScope::Base,
				fields: vec![PropertyKeyId::new(0)],
				physical: Some(index(3)?),
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
			label_index: (
				IndexId::new(4).ok_or_else(|| codec::invalid("invalid label index"))?,
				SchemaVersion::INITIAL,
			),
			adjacency_index: (
				IndexId::new(5).ok_or_else(|| codec::invalid("invalid adjacency index"))?,
				SchemaVersion::INITIAL,
			),
			membership: index(4)?,
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
		}],
	})
}

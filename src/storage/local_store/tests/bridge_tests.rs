//! Registry publication uses the ordinary storage failure and recovery seams.

use super::{
	GenerationFailureGuard,
	GenerationFailurePoint,
	TestDirectory,
	generation_directories,
};
use crate::{
	bridge::{
		BridgeDatabaseOptions,
		BridgeEntityRef,
		BridgeMappingRole,
		BridgeMappingState,
		BridgeMappingSubject,
		BridgeMirrorRequest,
	},
	changefeed::{ChangefeedConfig, ChangefeedSource},
	datastore::{ClassSchema, Datastore, LocalOptions},
	gql_status::FerrosGrynnResult,
	graph::{AtomKind, EdgeTopology, GraphKind},
	values::{Object, PropertyMap, RecordId},
};

/// Failed reservations and confirmations cannot publish unreachable roots.
#[test]
fn bridge_prepublication_failures_preserve_reservation_and_recovery() -> FerrosGrynnResult<()> {
	let directory = TestDirectory::new();
	let mut options = LocalOptions::default();
	options.buffer_pool_pages = 64;
	let store = Datastore::create_local(&directory.0, options)?;
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
	let link = tx.define_class(ClassSchema::new("link", AtomKind::Edge, [])?)?;
	let graph = tx.define_graph("g", GraphKind::Graph)?.id();
	let a: RecordId = "person:a".parse()?;
	let b: RecordId = "person:b".parse()?;
	let edge: RecordId = "link:r".parse()?;
	for record in [&a, &b, &edge] {
		tx.create_atom(record.clone(), PropertyMap::new())?;
	}
	let source = tx.attach_node(graph, &a, None, None)?;
	let target = tx.attach_node(graph, &b, None, None)?;
	tx.attach_edge(graph, &edge, None, EdgeTopology::directed([source], [target]), None, None)?;
	let database = tx.register_bridge_database(
		BridgeDatabaseOptions::new("sdb", "fake", "ns", "db")?
			.map_class("person", "profile")?
			.map_graph(graph, "relations")?,
	)?;
	tx.commit()?;
	let mut tx = store.begin();
	let policy = ChangefeedConfig::from_nanoseconds(86_400_000_000_000)?.with_full(true);
	tx.configure_changefeed(ChangefeedSource::Class(link), policy)?;
	tx.configure_changefeed(ChangefeedSource::Graph(graph), policy)?;
	tx.commit()?;
	let atom_entries = store.changefeed_statistics()?.net_entries;
	let subject = BridgeMappingSubject::Relation {
		graph,
		edge,
	};
	let external = BridgeEntityRef {
		database,
		record: "relations:r".parse()?,
	};
	let intent = BridgeMirrorRequest {
		relation: external.record.clone(),
		source: "profile:a".parse()?,
		target: "profile:b".parse()?,
		properties: Object::new(),
	};
	let points = [
		GenerationFailurePoint::AfterBuild,
		GenerationFailurePoint::AfterFileSync,
		GenerationFailurePoint::AfterCatalogStage,
		GenerationFailurePoint::BeforeWalCommit,
	];
	let baseline = generation_directories(&directory.0);
	for point in points {
		let mut tx = store.begin();
		let reference =
			tx.reserve_bridge_mapping(subject.clone(), external.clone(), intent.clone())?;
		let failure = GenerationFailureGuard::set(point);
		let result = tx.commit();
		drop(failure);
		assert!(result.is_err());
		assert!(store.begin().bridge_mapping(reference)?.is_none());
		assert!(store.begin().bridge_mapping_local(database, &subject)?.is_none());
		assert!(
			store
				.begin()
				.bridge_mapping_external(&external, BridgeMappingRole::Relation)?
				.is_none()
		);
		assert!(generation_directories(&directory.0) <= baseline);
		assert_eq!(store.changefeed_statistics()?.net_entries, atom_entries);
	}
	let mut tx = store.begin();
	let reference = tx.reserve_bridge_mapping(subject.clone(), external.clone(), intent)?;
	tx.commit()?;
	drop(store);
	let store = Datastore::open_local(&directory.0, options)?;
	let baseline = generation_directories(&directory.0);
	for point in points {
		let mut tx = store.begin();
		tx.confirm_bridge_mapping(reference)?;
		let failure = GenerationFailureGuard::set(point);
		let result = tx.commit();
		drop(failure);
		assert!(result.is_err());
		assert_eq!(
			store.begin().bridge_mapping(reference)?.map(|row| row.state()),
			Some(BridgeMappingState::Pending)
		);
		assert_eq!(
			store.begin().bridge_mapping_local(database, &subject)?.map(|row| row.reference()),
			Some(reference)
		);
		assert_eq!(
			store
				.begin()
				.bridge_mapping_external(&external, BridgeMappingRole::Relation)?
				.map(|row| row.reference()),
			Some(reference)
		);
		assert!(generation_directories(&directory.0) <= baseline);
	}
	drop(store);
	let store = Datastore::open_local(&directory.0, options)?;
	assert_eq!(
		store.begin().bridge_mapping(reference)?.map(|row| row.state()),
		Some(BridgeMappingState::Pending)
	);
	let mut tx = store.begin();
	tx.confirm_bridge_mapping(reference)?;
	tx.commit()?;
	drop(store);
	let store = Datastore::open_local(&directory.0, options)?;
	assert_eq!(
		store.begin().bridge_mapping(reference)?.map(|row| row.state()),
		Some(BridgeMappingState::Ready)
	);
	assert_eq!(store.changefeed_statistics()?.net_entries, atom_entries);
	Ok(())
}

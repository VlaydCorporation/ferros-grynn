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
	graph::{AtomKind, EdgeTopology, GraphKind},
	values::{Object, PropertyMap, RecordId},
};

/// Failed reservations and confirmations cannot publish unreachable roots.
#[test]
fn bridge_prepublication_failures_preserve_reservation_and_recovery() {
	use crate::test_assertions::AssertResult as _;

	let directory = TestDirectory::new();
	let options_defaults = LocalOptions::default();
	let options = LocalOptions {
		buffer_pool_pages: 64,
		..options_defaults
	};
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	let link =
		tx.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = tx.define_graph("g", GraphKind::Graph).assert_ok().id();
	let a: RecordId = "person:a".parse().assert_ok();
	let b: RecordId = "person:b".parse().assert_ok();
	let edge: RecordId = "link:r".parse().assert_ok();
	for record in [&a, &b, &edge] {
		tx.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let source = tx.attach_node(graph, &a, None, None).assert_ok();
	let target = tx.attach_node(graph, &b, None, None).assert_ok();
	tx.attach_edge(graph, &edge, None, EdgeTopology::directed([source], [target]), None, None)
		.assert_ok();
	let database = tx
		.register_bridge_database(
			BridgeDatabaseOptions::new("sdb", "fake", "ns", "db")
				.assert_ok()
				.map_class("person", "profile")
				.assert_ok()
				.map_graph(graph, "relations")
				.assert_ok(),
		)
		.assert_ok();
	tx.commit().assert_ok();
	let mut tx = store.begin();
	let policy = ChangefeedConfig::from_nanoseconds(86_400_000_000_000).assert_ok().with_full(true);
	tx.configure_changefeed(ChangefeedSource::Class(link), policy).assert_ok();
	tx.configure_changefeed(ChangefeedSource::Graph(graph), policy).assert_ok();
	tx.commit().assert_ok();
	let atom_entries = store.changefeed_statistics().assert_ok().net_entries;
	let subject = BridgeMappingSubject::Relation {
		graph,
		edge,
	};
	let external = BridgeEntityRef {
		database,
		record: "relations:r".parse().assert_ok(),
	};
	let intent = BridgeMirrorRequest {
		relation: external.record.clone(),
		source: "profile:a".parse().assert_ok(),
		target: "profile:b".parse().assert_ok(),
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
		let reference = tx
			.reserve_bridge_mapping(subject.clone(), external.clone(), intent.clone())
			.assert_ok();
		let failure = GenerationFailureGuard::set(point);
		let result = tx.commit();
		drop(failure);
		assert!(result.is_err());
		assert!(store.begin().bridge_mapping(reference).assert_ok().is_none());
		assert!(store.begin().bridge_mapping_local(database, &subject).assert_ok().is_none());
		assert!(
			store
				.begin()
				.bridge_mapping_external(&external, BridgeMappingRole::Relation)
				.assert_ok()
				.is_none()
		);
		assert!(generation_directories(&directory.0) <= baseline);
		assert_eq!(store.changefeed_statistics().assert_ok().net_entries, atom_entries);
	}
	let mut tx = store.begin();
	let reference =
		tx.reserve_bridge_mapping(subject.clone(), external.clone(), intent).assert_ok();
	tx.commit().assert_ok();
	drop(store);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	let baseline = generation_directories(&directory.0);
	for point in points {
		let mut tx = store.begin();
		tx.confirm_bridge_mapping(reference).assert_ok();
		let failure = GenerationFailureGuard::set(point);
		let result = tx.commit();
		drop(failure);
		assert!(result.is_err());
		assert_eq!(
			store.begin().bridge_mapping(reference).assert_ok().map(|row| row.state()),
			Some(BridgeMappingState::Pending)
		);
		assert_eq!(
			store
				.begin()
				.bridge_mapping_local(database, &subject)
				.assert_ok()
				.map(|row| row.reference()),
			Some(reference)
		);
		assert_eq!(
			store
				.begin()
				.bridge_mapping_external(&external, BridgeMappingRole::Relation)
				.assert_ok()
				.map(|row| row.reference()),
			Some(reference)
		);
		assert!(generation_directories(&directory.0) <= baseline);
	}
	drop(store);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	assert_eq!(
		store.begin().bridge_mapping(reference).assert_ok().map(|row| row.state()),
		Some(BridgeMappingState::Pending)
	);
	let mut tx = store.begin();
	tx.confirm_bridge_mapping(reference).assert_ok();
	tx.commit().assert_ok();
	drop(store);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	assert_eq!(
		store.begin().bridge_mapping(reference).assert_ok().map(|row| row.state()),
		Some(BridgeMappingState::Ready)
	);
	assert_eq!(store.changefeed_statistics().assert_ok().net_entries, atom_entries);
}

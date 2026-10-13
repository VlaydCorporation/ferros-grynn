//! Opt-in large-generation gate exercises public persistence above one WAL
//! frame.

use std::num::NonZeroUsize;

use ferros_grynn::{
	bridge::{
		BridgeDatabaseOptions,
		BridgeEntityRef,
		BridgeMappingState,
		BridgeMappingSubject,
		BridgeMirrorRequest,
	},
	datastore::{ClassSchema, Datastore, LocalOptions},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomKind, EdgeTopology, GraphKind},
	values::{Object, PropertyMap, RecordId, Value},
};

/// A registry generation over 256 MiB must publish by reachability, not frame
/// bytes.
#[test]
#[ignore = "writes a 300-MiB immutable generation; explicit Stage-30 recovery gate"]
fn bridge_large_generation_exceeds_wal_frame_and_reopens() -> FerrosGrynnResult<()> {
	let directory =
		std::env::temp_dir().join(format!("fgb30l-{:016x}", uuid::Uuid::now_v7().as_u128() as u64));
	let mut options = LocalOptions::default();
	options.buffer_pool_pages = 64;
	options.max_write_set_bytes = 4 * 1024 * 1024 * 1024;
	options.maintenance.max_memory_bytes = NonZeroUsize::new(1024 * 1024 * 1024)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("test memory budget"))?;
	let store = Datastore::create_local(&directory, options)?;
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
	tx.define_class(ClassSchema::new("link", AtomKind::Edge, [])?)?;
	let graph = tx.define_graph("g", GraphKind::Graph)?.id();
	let a: RecordId = "person:a".parse()?;
	let b: RecordId = "person:b".parse()?;
	tx.create_atom(a.clone(), PropertyMap::new())?;
	tx.create_atom(b.clone(), PropertyMap::new())?;
	let a = tx.attach_node(graph, &a, None, None)?;
	let b = tx.attach_node(graph, &b, None, None)?;
	let database = tx.register_bridge_database(
		BridgeDatabaseOptions::new("sdb", "fake", "ns", "db")?.map_graph(graph, "relation")?,
	)?;
	let mut references = Vec::new();
	for ordinal in 0..100_usize {
		let record = RecordId::from_value("link", Value::from(ordinal))?;
		tx.create_atom(record.clone(), PropertyMap::new())?;
		tx.attach_edge(graph, &record, None, EdgeTopology::directed([a], [b]), None, None)?;
		let relation = RecordId::from_value("relation", Value::from(ordinal))?;
		let reference = tx.reserve_bridge_mapping(
			BridgeMappingSubject::Relation {
				graph,
				edge: record,
			},
			BridgeEntityRef {
				database,
				record: relation.clone(),
			},
			BridgeMirrorRequest {
				relation,
				source: "profile:a".parse()?,
				target: "profile:b".parse()?,
				properties: Object::from_iter([(
					"payload",
					Value::from("x".repeat(3 * 1024 * 1024)),
				)]),
			},
		)?;
		references.push(reference);
	}
	tx.commit()?;
	drop(store);
	let heap_bytes = std::fs::read_dir(directory.join("atoms"))
		.map_err(|error| {
			FerrosGrynnError::bridge_operation_error("test generation enumeration failed")
				.with_technical_source(error)
		})?
		.map(|entry| {
			let entry = entry.map_err(|error| {
				FerrosGrynnError::bridge_operation_error("test generation entry failed")
					.with_technical_source(error)
			})?;
			let heap = entry.path().join("bridges/records.fgb");
			if !heap.exists() {
				return Ok(0_u64);
			}
			std::fs::metadata(heap).map(|meta| meta.len()).map_err(|error| {
				FerrosGrynnError::bridge_operation_error("test heap metadata failed")
					.with_technical_source(error)
			})
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?
		.into_iter()
		.max()
		.ok_or_else(|| FerrosGrynnError::bridge_operation_error("test heap absent"))?;
	assert!(heap_bytes > 256 * 1024 * 1024);
	let store = Datastore::open_local(&directory, options)?;
	let mut tx = store.begin();
	for reference in references {
		let row = tx
			.bridge_mapping(reference)?
			.ok_or_else(|| FerrosGrynnError::bridge_mapping_error("recovered mapping absent"))?;
		assert_eq!(row.state(), BridgeMappingState::Pending);
		assert_eq!(row.mirror().map(|intent| intent.properties.len()), Some(1));
	}
	drop(tx);
	drop(store);
	std::fs::remove_dir_all(directory).map_err(|error| {
		FerrosGrynnError::bridge_operation_error("test cleanup failed").with_technical_source(error)
	})?;
	Ok(())
}

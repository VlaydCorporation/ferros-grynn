use super::*;
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Base scalar addresses retain generations; Effective addresses remain local.
#[test]
fn scalar_indexes_read_reused_global_slots_after_reopen() {
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	let mut seed = store.begin();
	let graph = seed.define_graph("scope", GraphKind::Graph).assert_ok();
	let key = seed.intern_property("value").assert_ok();
	for (name, kind) in [("point_node", AtomKind::Node), ("point_edge", AtomKind::Edge)] {
		let mut class =
			ClassSchema::new(name, kind, [ClassField::new("value", Type::Int)]).assert_ok();
		for (suffix, kind, scope) in [
			("std", ClassIndexKind::Standard, IndexScope::Base),
			("uniq", ClassIndexKind::Unique, IndexScope::Base),
			("local", ClassIndexKind::Standard, IndexScope::Effective {
				graph: graph.id(),
			}),
		] {
			class = class
				.with_index(
					ClassIndex::new(format!("{name}_{suffix}"), kind, scope, ["value"]).assert_ok(),
				)
				.assert_ok();
		}
		seed.define_class(class).assert_ok();
		seed.create_atom(format!("{name}:retired").parse().assert_ok(), PropertyMap::new())
			.assert_ok();
	}
	seed.commit().assert_ok();
	let mut remove = store.begin();
	for name in ["point_node", "point_edge"] {
		remove.delete_atom(&format!("{name}:retired").parse().assert_ok()).assert_ok();
	}
	remove.commit().assert_ok();
	let mut create = store.begin();
	for name in ["point_node", "point_edge"] {
		let mut properties = PropertyMap::new();
		properties.insert(key, Value::from(7));
		create.create_atom(format!("{name}:current").parse().assert_ok(), properties).assert_ok();
	}
	let node = create
		.attach_node(graph.id(), &"point_node:current".parse().assert_ok(), None, None)
		.assert_ok();
	create
		.attach_edge(
			graph.id(),
			&"point_edge:current".parse().assert_ok(),
			None,
			EdgeTopology::directed([node], [node]),
			None,
			None,
		)
		.assert_ok();
	create.commit().assert_ok();
	for name in ["point_node", "point_edge"] {
		let record = format!("{name}:current").parse().assert_ok();
		let id = store.inner.current_root().atom_by_record(&record).assert_some().id();
		assert!(id.generation() > 0, "fixture must exercise a reused global slot");
		assert_ne!(id.raw(), node.raw());
	}
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);

	// The second reopen reads the sparse replacement from WAL, without a
	// checkpoint.
	for expected in [7, 8] {
		let store = Datastore::open_local(&directory.0, options).assert_ok();
		for name in ["point_node", "point_edge"] {
			let wanted = Value::Array(
				vec![Value::from(format!("{name}:current").parse::<RecordId>().assert_ok())].into(),
			);
			for suffix in ["std", "uniq", "local"] {
				let scope = if suffix == "local" {
					" IN GRAPH @scope"
				} else {
					""
				};
				let query = format!(
					"SELECT VALUE id FROM {name}{scope} WITH INDEX {name}_{suffix} WHERE value = \
					 {expected};"
				);
				assert_eq!(query_value(&store, &query, &Object::new()).assert_ok(), wanted);
			}
		}
		if expected == 7 {
			let mut write = store.begin();
			for name in ["point_node", "point_edge"] {
				write
					.set_base_property(
						&format!("{name}:current").parse().assert_ok(),
						key,
						Value::from(8),
					)
					.assert_ok();
			}
			write.commit().assert_ok();
		}
	}
}

/// Verifies effective search indexes match scan after local reopen.
#[test]
fn effective_search_indexes_match_scan_after_local_reopen() {
	use crate::test_assertions::AssertResult as _;

	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	let setup = store
		.execute(
			"DEFINE ANALYZER words TOKENIZERS blank FILTERS lowercase; DEFINE GRAPH research; \
			 DEFINE CLASS document TYPE NODE SCHEMAFULL; DEFINE FIELD title ON CLASS document \
			 TYPE string; DEFINE FIELD embedding ON CLASS document TYPE vector<f32>; CREATE \
			 document:a SET title = 'base first', embedding = $a; CREATE document:b SET title = \
			 'base second', embedding = $b; ATTACH document:a TO GRAPH research; ATTACH \
			 document:b TO GRAPH research; DEFINE INDEX title_effective ON CLASS document FIELDS \
			 title FULLTEXT ANALYZER words HIGHLIGHTS SCOPE EFFECTIVE IN GRAPH research; DEFINE \
			 INDEX hnsw_effective ON CLASS document FIELDS embedding HNSW DIMENSION 2 TYPE F32 \
			 DIST EUCLIDEAN EFC 16 M 4 SCOPE EFFECTIVE IN GRAPH research; DEFINE INDEX \
			 disk_effective ON CLASS document FIELDS embedding DISKANN DIMENSION 2 TYPE F32 DIST \
			 EUCLIDEAN DEGREE 4 L_BUILD 8 ALPHA 1.2 SCOPE EFFECTIVE IN GRAPH research; UPDATE \
			 document:b IN GRAPH research SET title = 'overlay target', embedding = $overlay;",
			&Object::from_iter([
				("a", Value::Vector(Vector::F32(vec![10.0, 0.0]))),
				("b", Value::Vector(Vector::F32(vec![20.0, 0.0]))),
				("overlay", Value::Vector(Vector::F32(vec![0.0, 0.0]))),
			]),
		)
		.assert_ok();
	if setup.iter().any(|result| result.result.is_err()) {
		return {
			let test_result: FerrosGrynnResult<()> = Err(FerrosGrynnError::invalid_argument(
				"effective search-index fixture setup failed",
			));
			test_result.assert_ok();
		};
	}
	let parameters = Object::from_iter([("query", Value::Vector(Vector::F32(vec![0.0, 0.0])))]);
	let queries = [
		(
			"SELECT VALUE id FROM document IN GRAPH @research WITH INDEX title_effective WHERE \
			 title @@ 'overlay' ORDER BY id;",
			"SELECT VALUE id FROM document IN GRAPH @research WITH NOINDEX WHERE title @@ \
			 'overlay' ORDER BY id;",
		),
		(
			"SELECT VALUE id FROM document IN GRAPH @research WITH INDEX hnsw_effective WHERE \
			 embedding <| 1, EUCLIDEAN |> $query;",
			"SELECT VALUE id FROM document IN GRAPH @research WITH NOINDEX WHERE embedding <| 1, \
			 EUCLIDEAN |> $query;",
		),
		(
			"SELECT VALUE id FROM document IN GRAPH @research WITH INDEX disk_effective WHERE \
			 embedding <| 1, EUCLIDEAN |> $query;",
			"SELECT VALUE id FROM document IN GRAPH @research WITH NOINDEX WHERE embedding <| 1, \
			 EUCLIDEAN |> $query;",
		),
	];
	let expected = queries
		.iter()
		.map(|(_, scan)| query_value(&store, scan, &parameters))
		.collect::<FerrosGrynnResult<Vec<_>>>()
		.assert_ok();
	let expected_id = Value::Array(
		vec![Value::from(
			"document:b"
				.parse::<RecordId>()
				.map_err(|error| FerrosGrynnError::invalid_argument(error.to_string()))
				.assert_ok(),
		)]
		.into(),
	);
	for ((forced, _), expected) in queries.iter().zip(&expected) {
		assert_eq!(expected, &expected_id);
		assert_eq!(&query_value(&store, forced, &parameters).assert_ok(), expected);
	}
	assert!(store.checkpoint(CheckpointMode::Restart).assert_ok());
	drop(store);

	let reopened = Datastore::open_local(&directory.0, options).assert_ok();
	for ((forced, _), expected) in queries.iter().zip(&expected) {
		assert_eq!(&query_value(&reopened, forced, &parameters).assert_ok(), expected);
	}
}

/// Returns query value for this module.
fn query_value(store: &Datastore, query: &str, parameters: &Object) -> FerrosGrynnResult<Value> {
	store
		.execute(query, parameters)?
		.into_iter()
		.next()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("query returned no statement result"))?
		.result
		.map(|outcome| outcome.into_parts().0)
}

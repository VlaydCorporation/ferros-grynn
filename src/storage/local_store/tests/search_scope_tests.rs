use super::*;
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

#[test]
fn effective_search_indexes_match_scan_after_local_reopen() -> FerrosGrynnResult<()> {
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options)?;
	let setup = store.execute(
		"DEFINE ANALYZER words TOKENIZERS blank FILTERS lowercase; DEFINE GRAPH research; DEFINE \
		 CLASS document TYPE NODE SCHEMAFULL; DEFINE FIELD title ON CLASS document TYPE string; \
		 DEFINE FIELD embedding ON CLASS document TYPE vector<f32>; CREATE document:a SET title = \
		 'base first', embedding = $a; CREATE document:b SET title = 'base second', embedding = \
		 $b; ATTACH document:a TO GRAPH research; ATTACH document:b TO GRAPH research; DEFINE \
		 INDEX title_effective ON CLASS document FIELDS title FULLTEXT ANALYZER words HIGHLIGHTS \
		 SCOPE EFFECTIVE IN GRAPH research; DEFINE INDEX hnsw_effective ON CLASS document FIELDS \
		 embedding HNSW DIMENSION 2 TYPE F32 DIST EUCLIDEAN EFC 16 M 4 SCOPE EFFECTIVE IN GRAPH \
		 research; DEFINE INDEX disk_effective ON CLASS document FIELDS embedding DISKANN \
		 DIMENSION 2 TYPE F32 DIST EUCLIDEAN DEGREE 4 L_BUILD 8 ALPHA 1.2 SCOPE EFFECTIVE IN \
		 GRAPH research; UPDATE document:b IN GRAPH research SET title = 'overlay target', \
		 embedding = $overlay;",
		&Object::from_iter([
			("a", Value::Vector(Vector::F32(vec![10.0, 0.0]))),
			("b", Value::Vector(Vector::F32(vec![20.0, 0.0]))),
			("overlay", Value::Vector(Vector::F32(vec![0.0, 0.0]))),
		]),
	)?;
	if setup.iter().any(|result| result.result.is_err()) {
		return Err(FerrosGrynnError::invalid_argument(
			"effective search-index fixture setup failed",
		));
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
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let expected_id = Value::Array(
		vec![Value::from(
			"document:b"
				.parse::<RecordId>()
				.map_err(|error| FerrosGrynnError::invalid_argument(error.to_string()))?,
		)]
		.into(),
	);
	for ((forced, _), expected) in queries.iter().zip(&expected) {
		assert_eq!(expected, &expected_id);
		assert_eq!(&query_value(&store, forced, &parameters)?, expected);
	}
	assert!(store.checkpoint(CheckpointMode::Restart)?);
	drop(store);

	let reopened = Datastore::open_local(&directory.0, options)?;
	for ((forced, _), expected) in queries.iter().zip(&expected) {
		assert_eq!(&query_value(&reopened, forced, &parameters)?, expected);
	}
	Ok(())
}

fn query_value(store: &Datastore, query: &str, parameters: &Object) -> FerrosGrynnResult<Value> {
	store
		.execute(query, parameters)?
		.into_iter()
		.next()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("query returned no statement result"))?
		.result
		.map(|outcome| outcome.into_parts().0)
}

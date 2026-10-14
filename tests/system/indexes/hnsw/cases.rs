use std::{
	collections::BTreeSet,
	io::{BufRead, BufReader},
};

use ferros_grynn::{
	datastore::{ClassIndex, ClassIndexKind, HnswIndexOptions, IndexScope, VectorDistance},
	values::{Object, Value, Vector, VectorKind},
};
use flate2::read::GzDecoder;

use crate::support::{Database, execute, record, search_hint, value};

/// Loads supplied JSON-lines vectors and validates the complete compressed
/// fixture.
fn vectors(data: &[u8], expected_rows: usize) -> Vec<Vec<f32>> {
	let rows = BufReader::new(GzDecoder::new(data))
		.lines()
		.map(|line| {
			let vector: Vec<f32> = serde_json::from_str(&line.unwrap()).unwrap();
			assert_eq!(vector.len(), 20);
			assert!(vector.iter().all(|component| component.is_finite()));
			vector
		})
		.collect::<Vec<_>>();
	assert_eq!(rows.len(), expected_rows);
	rows
}

/// Uses ordinary arithmetic as the recall oracle, independent of query
/// execution.
fn nearest(rows: &[Vec<f32>], query: &[f32], k: usize) -> Vec<Value> {
	let mut ranked = rows
		.iter()
		.enumerate()
		.map(|(id, vector)| {
			let squared = vector
				.iter()
				.zip(query)
				.map(|(left, right)| {
					let difference = f64::from(*left) - f64::from(*right);
					difference * difference
				})
				.sum::<f64>();
			(id, squared)
		})
		.collect::<Vec<_>>();
	ranked.sort_by(|left, right| left.1.total_cmp(&right.1).then_with(|| left.0.cmp(&right.0)));
	ranked
		.into_iter()
		.take(k)
		.map(|(id, _)| Value::from(record(&format!("embedding:v{id:05}"))))
		.collect()
}

/// Publishes an SDK HNSW definition after bulk loading, then measures known
/// neighbours.
fn recall(db: Database, rows: &[Vec<f32>], minimum_recall: f64) {
	execute(
		&db,
		"DEFINE CLASS embedding TYPE NODE SCHEMAFULL; DEFINE FIELD vector ON CLASS embedding TYPE \
		 vector<f32>;",
		&Object::new(),
	);
	let mut write = db.begin();
	let key = db.property_dictionary().get("vector").unwrap();
	for (id, vector) in rows.iter().enumerate() {
		let mut properties = ferros_grynn::values::PropertyMap::new();
		properties.insert(key, Value::Vector(Vector::F32(vector.clone())));
		write.create_atom(record(&format!("embedding:v{id:05}")), properties).unwrap();
	}
	write.commit().unwrap();
	let mut schema = db.begin();
	schema
		.define_index(
			"embedding",
			ClassIndex::new_hnsw(
				"by_vector",
				IndexScope::Base,
				"vector",
				HnswIndexOptions::new(
					VectorKind::F32,
					std::num::NonZeroU16::new(20).unwrap(),
					VectorDistance::Euclidean,
					std::num::NonZeroU16::new(12).unwrap(),
					std::num::NonZeroU16::new(128).unwrap(),
				)
				.unwrap(),
			)
			.unwrap(),
		)
		.unwrap();
	schema.commit().unwrap();
	let probes = [0, rows.len() / 3, rows.len() / 2, rows.len() - 1];
	db.assert_committed(|store| {
		assert_eq!(
			store
				.class("embedding")
				.unwrap()
				.indexes()
				.find(|index| index.name().as_str() == "by_vector")
				.unwrap()
				.kind(),
			ClassIndexKind::Hnsw
		);
		for probe in probes {
			let parameters =
				Object::from_iter([("query", Value::Vector(Vector::F32(rows[probe].clone())))]);
			let expected = nearest(rows, &rows[probe], 10);
			let scanned = value(
				store,
				"SELECT VALUE id FROM embedding WITH NOINDEX WHERE vector <| 10, EUCLIDEAN |> \
				 $query;",
				&parameters,
			);
			assert_eq!(scanned, Value::Array(expected.clone().into()));
			let result = value(
				store,
				&format!(
					"SELECT VALUE id FROM embedding {} WHERE vector <| 10, EF 128 |> $query;",
					search_hint(store, "by_vector")
				),
				&parameters,
			);
			let actual = result.as_array().unwrap().iter().cloned().collect::<Vec<_>>();
			assert_eq!(actual.len(), 10);
			assert_eq!(actual.iter().collect::<BTreeSet<_>>().len(), 10);
			assert_eq!(actual[0], expected[0]);
			let recall = actual.iter().filter(|id| expected.contains(id)).count() as f64 / 10.0;
			assert!(
				recall >= minimum_recall,
				"probe {probe}: recall@10={recall}, expected {minimum_recall}"
			);
			if rows.len() <= 128 {
				assert_eq!(actual, expected);
			}
		}
	});
}

#[test]
fn hnsw_5000_fixture_sample_matches_independent_euclidean_oracle() {
	let rows = vectors(include_bytes!("../../../data/hnsw-random-5000-20-euclidean.gz"), 5000);
	recall(database(), &rows[..128], 1.0);
}

#[test]
fn hnsw_9000_fixture_sample_matches_independent_euclidean_oracle() {
	let rows = vectors(include_bytes!("../../../data/hnsw-random-9000-20-euclidean.gz"), 9000);
	recall(database(), &rows[..128], 1.0);
}

#[test]
#[ignore = "full 5000-vector corpus; run explicitly for the search quality gate"]
fn hnsw_full_5000_fixture_recall_at_10_is_at_least_90_percent() {
	let rows = vectors(include_bytes!("../../../data/hnsw-random-5000-20-euclidean.gz"), 5000);
	recall(database(), &rows, 0.9);
}

#[test]
#[ignore = "full 9000-vector corpus; run explicitly for the search quality gate"]
fn hnsw_full_9000_fixture_recall_at_10_is_at_least_90_percent() {
	let rows = vectors(include_bytes!("../../../data/hnsw-random-9000-20-euclidean.gz"), 9000);
	recall(database(), &rows, 0.9);
}

#[test]
fn knn_rejects_wrong_query_dimension_without_changing_stored_vectors() {
	let db = database();
	execute(
		&db,
		"DEFINE CLASS embedding TYPE NODE SCHEMAFULL; DEFINE FIELD vector ON CLASS embedding TYPE \
		 vector<f32>; DEFINE INDEX by_vector ON CLASS embedding FIELDS vector HNSW DIMENSION 20 \
		 TYPE F32 DIST EUCLIDEAN EFC 32 M 4 SCOPE BASE; CREATE embedding:valid SET vector = \
		 $vector;",
		&Object::from_iter([("vector", Value::Vector(Vector::F32(vec![0.0; 20])))]),
	);
	let invalid = Object::from_iter([("vector", Value::Vector(Vector::F32(vec![0.0; 19])))]);
	assert!(
		db.execute(
			"SELECT VALUE id FROM embedding WITH NOINDEX WHERE vector <| 1, EUCLIDEAN |> $vector;",
			&invalid
		)
		.unwrap()[0]
			.result
			.is_err()
	);
	if db.local_storage_status().is_some() {
		assert!(
			db.execute("UPDATE embedding:valid SET vector = $vector;", &invalid).unwrap()[0]
				.result
				.is_err()
		);
	}
	db.assert_committed(|store| {
		let parameters = Object::from_iter([("query", Value::Vector(Vector::F32(vec![0.0; 20])))]);
		let key = store.property_dictionary().get("vector").unwrap();
		assert_eq!(
			store.begin().atom(&record("embedding:valid")).unwrap().unwrap().properties().get(key),
			Some(&Value::Vector(Vector::F32(vec![0.0; 20])))
		);
		assert_eq!(
			value(
				store,
				&format!(
					"SELECT VALUE id FROM embedding {} WHERE vector <| 1, EF 32 |> $query;",
					search_hint(store, "by_vector")
				),
				&parameters
			),
			Value::Array(vec![Value::from(record("embedding:valid"))].into())
		);
	});
}

#[test]
fn hnsw_updates_and_tombstones_rerank_results_without_changing_old_snapshots() {
	let db = database();
	execute(
		&db,
		"DEFINE CLASS embedding TYPE NODE SCHEMAFULL; DEFINE FIELD vector ON CLASS embedding TYPE \
		 vector<f32>; DEFINE INDEX by_vector ON CLASS embedding FIELDS vector HNSW DIMENSION 2 \
		 TYPE F32 DIST EUCLIDEAN EFC 32 M 4 SCOPE BASE;",
		&Object::new(),
	);
	for (id, x) in [("a", 0.0), ("b", 10.0), ("c", 20.0)] {
		execute(
			&db,
			&format!("CREATE embedding:{id} SET vector = $vector;"),
			&Object::from_iter([("vector", Value::Vector(Vector::F32(vec![x, 0.0])))]),
		);
	}
	let text = format!(
		"SELECT VALUE id FROM embedding {} WHERE vector <| 2, EF 32 |> $query;",
		search_hint(&db, "by_vector")
	);
	let query = text.as_str();
	let parameters = Object::from_iter([("query", Value::Vector(Vector::F32(vec![0.0, 0.0])))]);
	let before = Value::Array(
		vec![Value::from(record("embedding:a")), Value::from(record("embedding:b"))].into(),
	);
	let mut old = db.begin();
	assert_eq!(
		old.execute(query, &parameters).unwrap().remove(0).result.unwrap().into_parts().0,
		before
	);
	let key = db.property_dictionary().get("vector").unwrap();
	{
		let mut aborted = db.begin();
		aborted
			.set_base_property(
				&record("embedding:c"),
				key,
				Value::Vector(Vector::F32(vec![0.5, 0.0])),
			)
			.unwrap();
	}
	assert_eq!(value(&db, query, &parameters), before);
	let mut write = db.begin();
	write
		.set_base_property(&record("embedding:c"), key, Value::Vector(Vector::F32(vec![0.5, 0.0])))
		.unwrap();
	write.delete_atom(&record("embedding:a")).unwrap();
	write.commit().unwrap();
	assert_eq!(
		old.execute(query, &parameters).unwrap().remove(0).result.unwrap().into_parts().0,
		before
	);
	drop(old);
	let expected = Value::Array(
		vec![Value::from(record("embedding:c")), Value::from(record("embedding:b"))].into(),
	);
	db.assert_committed(|store| {
		assert_eq!(value(store, query, &parameters), expected);
		assert_eq!(
			value(
				store,
				"SELECT VALUE id FROM embedding WITH NOINDEX WHERE vector <| 2, EUCLIDEAN |> \
				 $query;",
				&parameters
			),
			expected
		);
	});
}

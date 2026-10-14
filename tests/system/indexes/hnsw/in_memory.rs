use crate::support::in_memory as database;

include!("cases.rs");

#[test]
fn forced_hnsw_hint_reports_that_ram_has_no_physical_binding() {
	let db = database();
	execute(
		&db,
		"DEFINE CLASS embedding TYPE NODE SCHEMAFULL; DEFINE FIELD vector ON CLASS embedding TYPE \
		 vector<f32>; DEFINE INDEX by_vector ON CLASS embedding FIELDS vector HNSW DIMENSION 2 \
		 TYPE F32 DIST EUCLIDEAN EFC 16 M 4 SCOPE BASE; CREATE embedding:a SET vector = $vector;",
		&Object::from_iter([("vector", Value::Vector(Vector::F32(vec![0.0, 0.0])))]),
	);
	let parameters = Object::from_iter([("query", Value::Vector(Vector::F32(vec![0.0, 0.0])))]);
	let result = db
		.execute(
			"SELECT VALUE id FROM embedding WITH INDEX by_vector WHERE vector <| 1, EF 16 |> \
			 $query;",
			&parameters,
		)
		.unwrap();
	assert_eq!(result[0].result.as_ref().unwrap_err().status().code().to_string(), "FG-42F03");
}

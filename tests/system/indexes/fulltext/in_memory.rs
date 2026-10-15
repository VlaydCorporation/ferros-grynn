use crate::support::in_memory as database;

include!("cases.rs");

/// Verifies forced fulltext hint reports that ram has no physical binding.
#[test]
fn forced_fulltext_hint_reports_that_ram_has_no_physical_binding() {
	let db = database();
	execute(
		&db,
		"DEFINE ANALYZER words TOKENIZERS blank FILTERS lowercase; DEFINE CLASS document TYPE \
		 NODE SCHEMAFULL; DEFINE FIELD title ON CLASS document TYPE string; DEFINE INDEX by_title \
		 ON CLASS document FIELDS title FULLTEXT ANALYZER words SCOPE BASE; CREATE document:a SET \
		 title = 'Rust';",
		&Object::new(),
	);
	let result = db
		.execute(
			"SELECT VALUE id FROM document WITH INDEX by_title WHERE title @@ 'rust';",
			&Object::new(),
		)
		.unwrap();
	assert_eq!(result[0].result.as_ref().unwrap_err().status().code().to_string(), "FG-42F03");
}

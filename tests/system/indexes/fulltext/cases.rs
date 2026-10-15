use std::collections::BTreeMap;

use ferros_grynn::values::{Object, Value};

use crate::support::{execute, record, search_hint, value};

/// Uses real dictionary entries as the independent expected lemma mapping.
fn dictionary_sample() -> Vec<(String, String)> {
	let entries = include_str!("../../../data/lemmatization-en.txt")
		.lines()
		.map(|line| {
			let (word, lemma) = line.split_once('\t').unwrap();
			assert!(!word.is_empty() && !lemma.is_empty());
			// The fixture stores canonical form first, inflected form second.
			(lemma.to_owned(), word.to_owned())
		})
		.collect::<Vec<_>>();
	assert!(entries.len() > 1000);
	let sample = entries
		.into_iter()
		.filter(|(word, lemma)| {
			word != lemma
				&& word.chars().all(|letter| letter.is_ascii_alphabetic())
				&& lemma.chars().all(|letter| letter.is_ascii_alphabetic())
		})
		.take(128)
		.collect::<Vec<_>>();
	assert_eq!(sample.len(), 128);
	sample
}

/// Verifies fulltext dictionary mapper returns expected inflections and matches
/// scan.
#[test]
fn fulltext_dictionary_mapper_returns_expected_inflections_and_matches_scan() {
	let db = database();
	let entries = dictionary_sample();
	let mapper = entries.iter().cloned().collect::<BTreeMap<_, _>>();
	let mapper = mapper
		.iter()
		.map(|(word, lemma)| format!("`{word}`: '{lemma}'"))
		.collect::<Vec<_>>()
		.join(", ");
	let definition = format!(
		"DEFINE ANALYZER english TOKENIZERS blank FILTERS lowercase, mapper({{{mapper}}}); DEFINE \
		 CLASS document TYPE NODE SCHEMAFULL; DEFINE FIELD title ON CLASS document TYPE string;",
	);
	execute(&db, &definition, &Object::new());
	let key = db.property_dictionary().get("title").unwrap();
	let mut write = db.begin();
	for (id, (word, _)) in entries.iter().enumerate() {
		let mut properties = ferros_grynn::values::PropertyMap::new();
		properties.insert(key, word.to_uppercase());
		write.create_atom(record(&format!("document:d{id:03}")), properties).unwrap();
	}
	write.commit().unwrap();
	execute(
		&db,
		"DEFINE INDEX by_title ON CLASS document FIELDS title FULLTEXT ANALYZER english BM25(1.2, \
		 0.75) HIGHLIGHTS SCOPE BASE;",
		&Object::new(),
	);
	db.assert_committed(|store| {
		for probe in [0, 37, 127] {
			let lemma = &entries[probe].1;
			let expected = entries
				.iter()
				.enumerate()
				.filter(|(_, entry)| &entry.1 == lemma)
				.map(|(id, _)| Value::from(record(&format!("document:d{id:03}"))))
				.collect::<Vec<_>>();
			assert!(!expected.is_empty());
			let parameters = Object::from_iter([("term", Value::from(lemma.as_str()))]);
			for hint in [search_hint(store, "by_title"), "WITH NOINDEX".to_owned()] {
				assert_eq!(
					value(
						store,
						&format!(
							"SELECT VALUE id FROM document {hint} WHERE title @@ $term ORDER BY \
							 id;"
						),
						&parameters
					),
					Value::Array(expected.clone().into())
				);
			}
		}
		assert!(
			value(
				store,
				&format!(
					"SELECT VALUE id FROM document {} WHERE title @@ 'zzzzmissingterm' ORDER BY \
					 id;",
					search_hint(store, "by_title")
				),
				&Object::new()
			)
			.as_array()
			.unwrap()
			.is_empty()
		);
	});
}

/// Verifies fulltext update delete and rollback preserve old snapshot results.
#[test]
fn fulltext_update_delete_and_rollback_preserve_old_snapshot_results() {
	let db = database();
	execute(
		&db,
		"DEFINE ANALYZER words TOKENIZERS blank FILTERS lowercase; DEFINE CLASS document TYPE \
		 NODE SCHEMAFULL; DEFINE FIELD title ON CLASS document TYPE string; DEFINE INDEX by_title \
		 ON CLASS document FIELDS title FULLTEXT ANALYZER words HIGHLIGHTS SCOPE BASE; CREATE \
		 document:a SET title = 'Rust graph'; CREATE document:b SET title = 'Other';",
		&Object::new(),
	);
	let text = format!(
		"SELECT VALUE id FROM document {} WHERE title @@ 'rust' ORDER BY id;",
		search_hint(&db, "by_title")
	);
	let forced = text.as_str();
	let mut old = db.begin();
	let before =
		old.execute(forced, &Object::new()).unwrap().remove(0).result.unwrap().into_parts().0;
	assert_eq!(before, Value::Array(vec![Value::from(record("document:a"))].into()));
	{
		let mut aborted = db.begin();
		let title = db.property_dictionary().get("title").unwrap();
		aborted
			.set_base_property(&record("document:b"), title, Value::from("Rust private"))
			.unwrap();
	}
	assert_eq!(value(&db, forced, &Object::new()), before);
	execute(
		&db,
		"UPDATE document:b SET title = 'Rust storage'; DELETE document:a;",
		&Object::new(),
	);
	assert_eq!(
		old.execute(forced, &Object::new()).unwrap().remove(0).result.unwrap().into_parts().0,
		before
	);
	drop(old);
	db.assert_committed(|store| {
		let expected = Value::Array(vec![Value::from(record("document:b"))].into());
		assert_eq!(value(store, forced, &Object::new()), expected);
		assert_eq!(
			value(store, &forced.replace("WITH INDEX by_title", "WITH NOINDEX"), &Object::new()),
			expected
		);
	});
}

/// Verifies fulltext unicode highlights refer to original text and scores match
/// scan.
#[test]
fn fulltext_unicode_highlights_refer_to_original_text_and_scores_match_scan() {
	let db = database();
	execute(
		&db,
		"DEFINE ANALYZER words TOKENIZERS blank FILTERS nfkc, lowercase, ngram(2, 3); DEFINE \
		 CLASS document TYPE NODE SCHEMAFULL; DEFINE FIELD title ON CLASS document TYPE string; \
		 DEFINE INDEX by_title ON CLASS document FIELDS title FULLTEXT ANALYZER words HIGHLIGHTS \
		 SCOPE BASE; CREATE document:a SET title = 'Rust ﬃeld notes';",
		&Object::new(),
	);
	let query = "SELECT id, search::score(7) AS score, search::highlight('[', ']', 7) AS snippet \
	             FROM document WITH INDEX by_title WHERE title @7@ 'ffi' ORDER BY id;";
	let text = query.replace("WITH INDEX by_title", &search_hint(&db, "by_title"));
	let query = text.as_str();
	let expected =
		value(&db, &query.replace("WITH INDEX by_title", "WITH NOINDEX"), &Object::new());
	let row = expected.as_array().unwrap().get(0).unwrap().as_object().unwrap();
	assert_eq!(row.get("snippet"), Some(&Value::from("Rust [ﬃeld] notes")));
	assert!(row.get("score").unwrap().to_string().parse::<f64>().unwrap() > 0.0);
	db.assert_committed(|store| assert_eq!(value(store, query, &Object::new()), expected));
}

/// Verifies effective fulltext scope indexes overlay without changing base
/// search.
#[test]
fn effective_fulltext_scope_indexes_overlay_without_changing_base_search() {
	let db = database();
	execute(
		&db,
		"DEFINE ANALYZER words TOKENIZERS blank FILTERS lowercase; DEFINE CLASS document TYPE \
		 NODE SCHEMAFULL; DEFINE FIELD title ON CLASS document TYPE string; DEFINE GRAPH \
		 selected; CREATE document:a SET title = 'base'; ATTACH document:a TO GRAPH selected; \
		 DEFINE INDEX effective_title ON CLASS document FIELDS title FULLTEXT ANALYZER words \
		 SCOPE EFFECTIVE IN GRAPH selected; UPDATE document:a IN GRAPH selected SET title = \
		 'overlay';",
		&Object::new(),
	);
	db.assert_committed(|store| {
		let query = "SELECT VALUE id FROM document IN GRAPH @selected WITH INDEX effective_title \
		             WHERE title @@ 'overlay';";
		let text =
			query.replace("WITH INDEX effective_title", &search_hint(store, "effective_title"));
		let query = text.as_str();
		assert_eq!(
			value(store, query, &Object::new()),
			Value::Array(vec![Value::from(record("document:a"))].into())
		);
		assert_eq!(
			value(store, "SELECT VALUE title FROM document;", &Object::new()),
			Value::Array(vec![Value::from("base")].into())
		);
	});
}

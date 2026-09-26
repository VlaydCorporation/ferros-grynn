#![allow(
	clippy::expect_used,
	clippy::unwrap_used,
	reason = "integration tests fail immediately when public datastore operations return an error"
)]

#[cfg(all(test, windows))]
mod common;

#[cfg(all(test, windows))]
mod tests {
	use std::{
		fs::OpenOptions,
		io::{Read, Seek, SeekFrom, Write},
		path::{Path, PathBuf},
	};

	#[cfg(feature = "native-async-io")]
	use ferros_grynn::datastore::IoMode;
	use ferros_grynn::{
		datastore::{
			CheckpointMode,
			ClassField,
			ClassIndex,
			ClassIndexKind,
			ClassSchema,
			Datastore,
			IndexScope,
			LocalOptions,
		},
		graph::{AtomId, AtomKind, GraphKind, GraphRef, GraphStorageMode},
		values::{Object, PropertyMap, RecordId, RecordIdPart, Type, Uuid, Value},
	};

	use super::common::{
		assert_people_graph,
		commit_people_graph,
		create_uncommitted_graph,
		exercise_normalized_atom_model,
	};

	/// Unique database directory removed after a local integration test.
	struct LocalTestDirectory(PathBuf);

	impl LocalTestDirectory {
		/// Reserves a unique path without creating it before `create_local`.
		fn new() -> Self {
			Self(
				std::env::temp_dir()
					.join(format!("fgdb-{:016x}", Uuid::now_v7().as_inner().as_u128() as u64)),
			)
		}

		/// Returns the database path.
		fn path(&self) -> &Path {
			&self.0
		}
	}

	impl Drop for LocalTestDirectory {
		fn drop(&mut self) {
			drop(std::fs::remove_dir_all(&self.0));
		}
	}

	/// Keeps each local test's page cache small.
	fn local_options() -> LocalOptions {
		let mut options = LocalOptions::default();
		options.buffer_pool_pages = 16;
		options
	}

	#[test]
	fn commit_survives_close_and_reopen() {
		let directory = LocalTestDirectory::new();
		let options = local_options();
		let store = Datastore::create_local(directory.path(), options).unwrap();
		let (reference, alice, name) = commit_people_graph(&store);
		assert_eq!(reference.storage_mode(), GraphStorageMode::DiskBacked);
		drop(store);

		let reopened = Datastore::open_local(directory.path(), options).unwrap();
		assert_eq!(reopened.property_dictionary().get("name"), Some(name));
		assert_people_graph(&reopened, &reference, &alice, name);
	}

	#[test]
	fn dropped_transaction_is_not_recovered() {
		let directory = LocalTestDirectory::new();
		let options = local_options();
		let store = Datastore::create_local(directory.path(), options).unwrap();
		let reference = create_uncommitted_graph(&store);
		assert!(GraphRef::read(&reference, &store.begin()).is_err());
		drop(store);

		let reopened = Datastore::open_local(directory.path(), options).unwrap();
		assert!(GraphRef::read(&reference, &reopened.begin()).is_err());
	}

	#[test]
	fn normalized_atom_model_survives_restart_checkpoint_and_reopen() {
		let directory = LocalTestDirectory::new();
		let options = local_options();
		let store = Datastore::create_local(directory.path(), options).unwrap();
		let (remaining_graph, alice, name) = exercise_normalized_atom_model(&store);
		assert!(store.checkpoint(CheckpointMode::Restart).unwrap());
		drop(store);

		let reopened = Datastore::open_local(directory.path(), options).unwrap();
		assert_people_graph_atom(&reopened, &remaining_graph, &alice, name);
	}

	#[test]
	fn atomic_import_generation_survives_reopen_without_checkpoint() {
		let directory = LocalTestDirectory::new();
		let options = local_options();
		let store = Datastore::create_local(directory.path(), options).unwrap();
		let mut schema = store.begin();
		schema.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
		let graph = schema.define_graph("social", GraphKind::Graph).unwrap();
		schema.commit().unwrap();

		let results = store
			.execute(
				"BEGIN IMPORT INTO GRAPH social WITH (SEGMENT SIZE = 1); CREATE person:alice; \
				 ATTACH person:alice LABEL Person; CREATE person:bob; ATTACH person:bob; END \
				 IMPORT WITH VALIDATION;",
				&Object::new(),
			)
			.unwrap();
		assert!(results[0].result.is_ok());
		drop(store);

		let reopened = Datastore::open_local(directory.path(), options).unwrap();
		let alice = "person:alice".parse::<RecordId>().unwrap();
		let bob = "person:bob".parse::<RecordId>().unwrap();
		let mut read = reopened.begin();
		assert!(read.atom(&alice).unwrap().is_some());
		assert!(read.atom(&bob).unwrap().is_some());
		assert!(read.membership(graph.id(), &alice).unwrap().is_some());
		assert!(read.membership(graph.id(), &bob).unwrap().is_some());
	}

	#[test]
	fn gql_transaction_and_control_state_survive_reopen() {
		let directory = LocalTestDirectory::new();
		let options = local_options();
		let store = Datastore::create_local(directory.path(), options).unwrap();
		let mut session = store.session();
		assert!(session.execute("BEGIN;", &Object::new()).unwrap()[0].result.is_ok());
		let setup = session
			.execute(
				"DEFINE CLASS person TYPE NODE; DEFINE FIELD version ON CLASS person TYPE int; \
				 CREATE person:alice SET version = 1;",
				&Object::new(),
			)
			.unwrap();
		assert!(setup.iter().all(|result| result.result.is_ok()));
		let caught = session
			.execute(
				"TRY { UPDATE person:alice SET version = 2; THROW 'rollback'; } CATCH |$e| { \
				 $e.message; }; ASSERT INVARIANTS;",
				&Object::new(),
			)
			.unwrap();
		assert_eq!(caught[0].result.as_ref().unwrap().value(), &Value::from("rollback"));
		assert!(caught[1].result.is_ok());
		assert!(
			session.execute("COMMIT WITH VALIDATION;", &Object::new()).unwrap()[0].result.is_ok()
		);
		drop(session);
		drop(store);

		let reopened = Datastore::open_local(directory.path(), options).unwrap();
		let rows =
			reopened.execute("SELECT VALUE version FROM person:alice;", &Object::new()).unwrap();
		let Value::Array(values) = rows[0].result.as_ref().unwrap().value() else {
			panic!("SELECT VALUE must return an array");
		};
		assert_eq!(values.as_slice(), &[Value::from(1_i64)]);
		assert_eq!(
			reopened.execute("FOR $x IN 1..=3 { $x * 2; };", &Object::new()).unwrap()[0]
				.result
				.as_ref()
				.unwrap()
				.value(),
			&Value::from(6_i64)
		);
	}

	#[test]
	fn optimizer_plan_and_deterministic_result_survive_reopen() {
		fn observe(store: &Datastore) -> (Value, Value) {
			let mut session = store.session();
			session
				.execute(
					"BEGIN ISOLATION LEVEL SNAPSHOT EXECUTION MODE DETERMINISTIC;",
					&Object::new(),
				)
				.unwrap();
			let result = session
				.execute(
					"SELECT VALUE id FROM person WHERE rank >= 1 ORDER BY rank ASC NULLS FIRST;",
					&Object::new(),
				)
				.unwrap()
				.remove(0)
				.result
				.unwrap()
				.into_parts()
				.0;
			let explain = session
				.execute(
					"EXPLAIN FULL FORMAT JSON SELECT VALUE id FROM person WHERE rank >= 1 ORDER \
					 BY rank ASC NULLS FIRST;",
					&Object::new(),
				)
				.unwrap()
				.remove(0)
				.result
				.unwrap()
				.into_parts()
				.0;
			let Value::Object(explain) = explain else {
				panic!("EXPLAIN JSON must return an object");
			};
			let hash = explain.get("plan_hash").cloned().unwrap();
			session.execute("CANCEL;", &Object::new()).unwrap();
			(result, hash)
		}

		let directory = LocalTestDirectory::new();
		let options = local_options();
		let store = Datastore::create_local(directory.path(), options).unwrap();
		store
			.execute(
				"DEFINE CLASS person TYPE NODE SCHEMAFULL; DEFINE FIELD rank ON CLASS person TYPE \
				 int; DEFINE INDEX by_rank ON CLASS person FIELDS rank SCOPE BASE; CREATE \
				 person:z SET rank = 1; CREATE person:a SET rank = 1; CREATE person:m SET rank = \
				 2;",
				&Object::new(),
			)
			.unwrap();
		let before = observe(&store);
		drop(store);

		let reopened = Datastore::open_local(directory.path(), options).unwrap();
		assert_eq!(observe(&reopened), before);
	}

	#[test]
	fn atomic_import_streams_every_index_family_and_reopens() {
		let directory = LocalTestDirectory::new();
		let options = local_options();
		let store = Datastore::create_local(directory.path(), options).unwrap();
		let mut schema = store.begin();
		let graph = schema.define_graph("social", GraphKind::Graph).unwrap();
		let person = ClassSchema::new("person", AtomKind::Node, [
			ClassField::new("age", Type::Int),
			ClassField::new("email", Type::String),
		])
		.unwrap()
		.with_index(
			ClassIndex::new("by_age", ClassIndexKind::Standard, IndexScope::Base, ["age"]).unwrap(),
		)
		.unwrap()
		.with_index(
			ClassIndex::new("unique_email", ClassIndexKind::Unique, IndexScope::Base, ["email"])
				.unwrap(),
		)
		.unwrap()
		.with_index(
			ClassIndex::new(
				"base_count",
				ClassIndexKind::Count,
				IndexScope::Base,
				std::iter::empty::<&str>(),
			)
			.unwrap(),
		)
		.unwrap()
		.with_index(
			ClassIndex::new(
				"effective_age",
				ClassIndexKind::Standard,
				IndexScope::Effective {
					graph: graph.id(),
				},
				["age"],
			)
			.unwrap(),
		)
		.unwrap()
		.with_index(
			ClassIndex::new(
				"effective_email",
				ClassIndexKind::Unique,
				IndexScope::Effective {
					graph: graph.id(),
				},
				["email"],
			)
			.unwrap(),
		)
		.unwrap()
		.with_index(
			ClassIndex::new(
				"effective_count",
				ClassIndexKind::Count,
				IndexScope::Effective {
					graph: graph.id(),
				},
				std::iter::empty::<&str>(),
			)
			.unwrap(),
		)
		.unwrap();
		schema.define_class(person).unwrap();
		schema.commit().unwrap();
		let age = store.property_dictionary().get("age").unwrap();
		let email = store.property_dictionary().get("email").unwrap();

		let results = store
			.execute(
				"BEGIN IMPORT INTO GRAPH social; CREATE person:alice SET age = 42, email = \
				 'alice@example.test'; CREATE person:bob SET age = 42, email = \
				 'bob@example.test'; ATTACH person:alice LABEL Person; ATTACH person:bob LABEL \
				 Person; END IMPORT WITH VALIDATION;",
				&Object::new(),
			)
			.unwrap();
		let Value::Object(report) = results[0].result.as_ref().unwrap().value() else {
			panic!("import must return a structured report");
		};
		let timings = report
			.get("index_timings_ns")
			.and_then(Value::as_object)
			.expect("local import must expose physical index timings");
		assert_eq!(timings.len(), 6);
		drop(store);

		let reopened = Datastore::open_local(directory.path(), options).unwrap();
		assert_eq!(find_files(directory.path(), "std_0000.fgidx").len(), 2);
		assert_eq!(find_files(directory.path(), "uniq_0000.fgidx").len(), 2);
		assert_eq!(find_files_starting_with(directory.path(), "cnt_").len(), 2);
		assert_eq!(find_files(directory.path(), "lbl_node_0.fgidx").len(), 1);
		assert_eq!(find_files(directory.path(), "primary.fgidx").len(), 1);
		assert_eq!(find_files(directory.path(), "memberships.fgidx").len(), 1);
		assert_eq!(find_files(directory.path(), "membership.fgidx").len(), 1);

		let alice = "person:alice".parse::<RecordId>().unwrap();
		let mut read = reopened.begin();
		let atom = read.atom(&alice).unwrap().unwrap();
		assert_eq!(atom.properties().get(age), Some(&Value::from(42_i64)));
		assert_eq!(atom.properties().get(email), Some(&Value::from("alice@example.test")),);
		assert!(read.membership(graph.id(), &alice).unwrap().is_some());
	}

	#[test]
	fn atomic_import_all_dml_matches_ram_before_and_after_reopen() {
		let memory = Datastore::in_memory();
		let memory_graph = prepare_all_dml_import(&memory);
		run_all_dml_import(&memory);
		let expected = observe_all_dml_import(&memory, memory_graph.id());

		let directory = LocalTestDirectory::new();
		let options = local_options();
		let local = Datastore::create_local(directory.path(), options).unwrap();
		let local_graph = prepare_all_dml_import(&local);
		run_all_dml_import(&local);
		assert_eq!(observe_all_dml_import(&local, local_graph.id()), expected);
		drop(local);

		let reopened = Datastore::open_local(directory.path(), options).unwrap();
		assert_eq!(observe_all_dml_import(&reopened, local_graph.id()), expected);
	}

	#[test]
	fn import_segments_do_not_stream_items_into_the_wal() {
		let run = |items: u64| {
			let directory = LocalTestDirectory::new();
			let options = local_options();
			let store = Datastore::create_local(directory.path(), options).unwrap();
			let mut schema = store.begin();
			schema.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
			schema.commit().unwrap();
			let before = directory_bytes(&directory.path().join("wal"));
			let mut import_options = ferros_grynn::datastore::ImportOptions::default();
			import_options.segment_size = 1;
			let report = store
				.import(import_options, |session| {
					for item in 0..items {
						session.apply_item(|transaction| {
							transaction.create_atom(
								RecordId::new(
									"person",
									RecordIdPart::Int(i64::try_from(item).unwrap()),
								)
								.unwrap(),
								PropertyMap::new(),
							)
						})?;
					}
					Ok(())
				})
				.unwrap();
			let wal_bytes = directory_bytes(&directory.path().join("wal")) - before;
			(wal_bytes, report.segments())
		};

		let (single_wal, single_segments) = run(1);
		let (many_wal, many_segments) = run(64);
		assert_eq!(single_segments, 1);
		assert_eq!(many_segments, 64);
		assert_eq!(many_wal, single_wal);
	}

	#[test]
	fn statistics_auxiliary_files_are_not_read_during_open() {
		let directory = LocalTestDirectory::new();
		let options = local_options();
		let store = Datastore::create_local(directory.path(), options).unwrap();
		commit_people_graph(&store);
		assert!(store.checkpoint(CheckpointMode::Restart).unwrap());
		drop(store);

		let label_statistics = find_file(directory.path(), "label_stats.fgb").unwrap();
		assert!(find_file(directory.path(), "class_stats.fgb").is_some());
		assert!(find_file(directory.path(), "prop_stats.fgb").is_some());
		corrupt_byte(&label_statistics, 128);

		let reopened = Datastore::open_local(directory.path(), options).unwrap();
		assert!(reopened.class("person").is_some());
	}

	#[test]
	fn startup_removes_stale_import_sort_sessions() {
		let directory = LocalTestDirectory::new();
		let options = local_options();
		let store = Datastore::create_local(directory.path(), options).unwrap();
		drop(store);
		let scratch = directory.path().join("tmp").join("index-sort").join("stale-session");
		std::fs::create_dir_all(&scratch).unwrap();
		std::fs::write(scratch.join("0000000000000000.run"), b"incomplete").unwrap();

		let reopened = Datastore::open_local(directory.path(), options).unwrap();
		assert!(!scratch.exists());
		drop(reopened);
	}

	#[test]
	fn corrupted_import_primary_is_rejected_on_reopen() {
		let directory = LocalTestDirectory::new();
		let options = local_options();
		let store = Datastore::create_local(directory.path(), options).unwrap();
		let mut schema = store.begin();
		schema.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
		schema.commit().unwrap();
		store
			.execute(
				"BEGIN IMPORT; CREATE person:alice; END IMPORT WITH VALIDATION;",
				&Object::new(),
			)
			.unwrap();
		drop(store);

		let reopened = Datastore::open_local(directory.path(), options).unwrap();
		drop(reopened);
		let primary = find_file(directory.path(), "primary.fgidx").unwrap();
		corrupt_byte(&primary, 128);
		assert!(Datastore::open_local(directory.path(), options).is_err());
	}

	#[cfg(feature = "native-async-io")]
	#[test]
	fn native_async_normalized_model_survives_reopen() {
		let directory = LocalTestDirectory::new();
		let mut options = local_options();
		options.io_mode = IoMode::NativeAsync;
		let store = Datastore::create_local(directory.path(), options).unwrap();
		let (remaining_graph, alice, name) = exercise_normalized_atom_model(&store);
		drop(store);

		let reopened = Datastore::open_local(directory.path(), options).unwrap();
		assert_people_graph_atom(&reopened, &remaining_graph, &alice, name);
	}

	fn assert_people_graph_atom(
		store: &Datastore,
		graph: &GraphRef,
		alice: &ferros_grynn::graph::AtomRef,
		name: ferros_grynn::values::PropertyKeyId,
	) {
		let mut transaction = store.begin();
		assert!(graph.read(&transaction).is_ok());
		assert_eq!(
			transaction.effective_property(graph.id(), alice.id(), name).unwrap(),
			Some("Updated".into())
		);
	}

	fn prepare_all_dml_import(store: &Datastore) -> GraphRef {
		let mut schema = store.begin();
		schema.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
		schema.define_class(ClassSchema::new("friendship", AtomKind::Edge, []).unwrap()).unwrap();
		let graph = schema.define_graph("social", GraphKind::Graph).unwrap();
		schema.commit().unwrap();
		graph
	}

	fn run_all_dml_import(store: &Datastore) {
		let results = store
			.execute(
				"BEGIN IMPORT INTO GRAPH social WITH (SEGMENT SIZE = 2); CREATE person:alice; \
				 CREATE person:bob; CREATE person:remove; UPDATE person:alice SET active = true; \
				 UPSERT person:carol SET active = true; ATTACH person:alice LABEL Person; DETACH \
				 person:alice; ATTACH person:alice; RELATE \
				 (person:alice)-[friendship:known]->(person:bob); DELETE person:remove; END \
				 IMPORT WITH VALIDATION;",
				&Object::new(),
			)
			.unwrap();
		assert!(results[0].result.is_ok());
	}

	fn observe_all_dml_import(
		store: &Datastore,
		graph: ferros_grynn::graph::GraphId,
	) -> Vec<(Option<PropertyMap>, Option<AtomId>)> {
		let mut read = store.begin();
		["person:alice", "person:bob", "person:carol", "person:remove", "friendship:known"]
			.into_iter()
			.map(|record| {
				let record = record.parse::<RecordId>().unwrap();
				let properties = read.atom(&record).unwrap().map(|atom| atom.properties().clone());
				let membership = read.membership(graph, &record).unwrap();
				(properties, membership)
			})
			.collect()
	}

	fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
		find_files(root, name).into_iter().next()
	}

	fn corrupt_byte(path: &Path, offset: u64) {
		let mut file = OpenOptions::new().read(true).write(true).open(path).unwrap();
		let mut byte = [0_u8; 1];
		file.seek(SeekFrom::Start(offset)).unwrap();
		file.read_exact(&mut byte).unwrap();
		byte[0] ^= u8::MAX;
		file.seek(SeekFrom::Start(offset)).unwrap();
		file.write_all(&byte).unwrap();
		file.sync_all().unwrap();
	}

	fn find_files(root: &Path, name: &str) -> Vec<PathBuf> {
		find_matching_files(root, |candidate| candidate == name)
	}

	fn find_files_starting_with(root: &Path, prefix: &str) -> Vec<PathBuf> {
		find_matching_files(root, |candidate| candidate.starts_with(prefix))
	}

	fn find_matching_files(root: &Path, mut matches: impl FnMut(&str) -> bool) -> Vec<PathBuf> {
		let mut directories = vec![root.to_path_buf()];
		let mut matches_paths = Vec::new();
		while let Some(directory) = directories.pop() {
			let Ok(entries) = std::fs::read_dir(directory) else {
				continue;
			};
			for entry in entries.flatten() {
				let path = entry.path();
				if path.is_dir() {
					directories.push(path);
				} else if path
					.file_name()
					.and_then(|value| value.to_str())
					.is_some_and(&mut matches)
				{
					matches_paths.push(path);
				}
			}
		}
		matches_paths
	}

	fn directory_bytes(root: &Path) -> u64 {
		let mut total = 0_u64;
		let mut directories = vec![root.to_path_buf()];
		while let Some(directory) = directories.pop() {
			let Ok(entries) = std::fs::read_dir(directory) else {
				continue;
			};
			for entry in entries.flatten() {
				let path = entry.path();
				if path.is_dir() {
					directories.push(path);
				} else if let Ok(metadata) = entry.metadata() {
					total = total.saturating_add(metadata.len());
				}
			}
		}
		total
	}
}

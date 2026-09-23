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
		datastore::{CheckpointMode, Datastore, LocalOptions},
		graph::{GraphRef, GraphStorageMode},
		values::Uuid,
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
		let mut file = OpenOptions::new().read(true).write(true).open(label_statistics).unwrap();
		let mut byte = [0_u8; 1];
		file.seek(SeekFrom::Start(128)).unwrap();
		file.read_exact(&mut byte).unwrap();
		byte[0] ^= u8::MAX;
		file.seek(SeekFrom::Start(128)).unwrap();
		file.write_all(&byte).unwrap();
		file.sync_all().unwrap();
		drop(file);

		let reopened = Datastore::open_local(directory.path(), options).unwrap();
		assert!(reopened.class("person").is_some());
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

	fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
		let mut directories = vec![root.to_path_buf()];
		while let Some(directory) = directories.pop() {
			for entry in std::fs::read_dir(directory).ok()? {
				let path = entry.ok()?.path();
				if path.is_dir() {
					directories.push(path);
				} else if path.file_name().and_then(|value| value.to_str()) == Some(name) {
					return Some(path);
				}
			}
		}
		None
	}
}

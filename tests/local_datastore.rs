#![allow(
	clippy::expect_used,
	clippy::unwrap_used,
	reason = "integration tests fail immediately when public datastore operations return an error"
)]

#[cfg(all(test, windows))]
mod common;

#[cfg(all(test, windows))]
mod tests {
	use std::path::{Path, PathBuf};

	use ferros_grynn::{
		datastore::{Datastore, LocalOptions},
		graph::{GraphRef, GraphStorageMode},
		values::Uuid,
	};

	use super::common::{assert_people_graph, commit_people_graph, create_uncommitted_graph};

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
}

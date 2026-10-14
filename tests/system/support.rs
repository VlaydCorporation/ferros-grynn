use std::ops::Deref;
#[cfg(windows)]
use std::path::PathBuf;

#[cfg(windows)]
use ferros_grynn::datastore::{CheckpointMode, IoMode, LocalOptions};
use ferros_grynn::{
	datastore::{ClassField, ClassSchema, Datastore},
	graph::AtomKind,
	values::{Object, PropertyKeyId, PropertyMap, RecordId, Type, Value},
};

/// Owns the datastore before its temporary directory, including during
/// unwinding.
pub struct Database {
	store: Datastore,
	#[cfg(windows)]
	directory: Option<Directory>,
	#[cfg(windows)]
	options: LocalOptions,
}

impl Deref for Database {
	type Target = Datastore;

	fn deref(&self) -> &Self::Target {
		&self.store
	}
}

/// Creates the same public test boundary without filesystem state.
pub fn in_memory() -> Database {
	Database {
		store: Datastore::in_memory(),
		#[cfg(windows)]
		directory: None,
		#[cfg(windows)]
		options: LocalOptions::default(),
	}
}

#[cfg(windows)]
/// Creates an isolated synchronous Direct I/O database with a small page cache.
pub fn local() -> Database {
	local_with(IoMode::Sync)
}

#[cfg(all(windows, feature = "native-async-io"))]
/// Creates the same durable fixture using the production native I/O mode.
pub fn local_native_async() -> Database {
	local_with(IoMode::NativeAsync)
}

#[cfg(windows)]
/// Keeps local fixture ownership and budgets identical across I/O modes.
fn local_with(io_mode: IoMode) -> Database {
	let directory = Directory(std::env::temp_dir().join(format!(
		"fgdb-{:016x}",
		ferros_grynn::values::Uuid::now_v7().as_inner().as_u128() as u64
	)));
	let mut options = LocalOptions::default();
	options.io_mode = io_mode;
	options.buffer_pool_pages = 16;
	let store = Datastore::create_local(&directory.0, options).unwrap();
	assert_eq!(store.local_storage_status().unwrap().options().io_mode, io_mode);
	Database {
		store,
		directory: Some(directory),
		options,
	}
}

impl Database {
	/// Checks committed state; local repeats the check after WAL and checkpoint
	/// recovery.
	pub fn assert_committed(self, check: impl Fn(&Datastore)) {
		check(&self.store);
		#[cfg(windows)]
		if let Some(directory) = self.directory {
			drop(self.store);
			let reopened = Datastore::open_local(&directory.0, self.options).unwrap();
			assert_eq!(
				reopened.local_storage_status().unwrap().options().io_mode,
				self.options.io_mode
			);
			check(&reopened);
			// Ensure Restart has fresh WAL frames even for a read-only
			// scenario.
			let mut write = reopened.begin();
			write.define_graph("checkpoint_marker", ferros_grynn::graph::GraphKind::Graph).unwrap();
			write.commit().unwrap();
			assert!(reopened.checkpoint(CheckpointMode::Restart).unwrap());
			drop(reopened);
			let checkpointed = Datastore::open_local(&directory.0, self.options).unwrap();
			assert_eq!(
				checkpointed.local_storage_status().unwrap().options().io_mode,
				self.options.io_mode
			);
			check(&checkpointed);
		}
	}
}

#[cfg(windows)]
/// Deletes only the unique directory owned by this fixture.
struct Directory(PathBuf);

#[cfg(windows)]
impl Drop for Directory {
	fn drop(&mut self) {
		if let Err(error) = std::fs::remove_dir_all(&self.0) {
			eprintln!("could not remove test database {}: {error}", self.0.display());
		}
	}
}

/// Executes all statements and checks each result, including setup statements.
pub fn execute(store: &Datastore, text: &str, parameters: &Object) {
	let results = store.execute(text, parameters).unwrap();
	assert!(!results.is_empty(), "no statement results: {text}");
	for result in results {
		assert!(result.result.is_ok(), "query {text}: {:?}", result.result);
	}
}

/// Returns a single owned result through the downstream SDK boundary.
pub fn value(store: &Datastore, text: &str, parameters: &Object) -> Value {
	let mut results = store.execute(text, parameters).unwrap();
	assert_eq!(results.len(), 1, "expected one statement: {text}");
	results.remove(0).result.unwrap().into_parts().0
}

/// Parses a stable logical identity, never a materialization-local AtomId.
pub fn record(text: &str) -> RecordId {
	text.parse().unwrap()
}

/// Forces physical search in local databases; RAM uses the automatic exact
/// fallback.
pub fn search_hint(store: &Datastore, index: &str) -> String {
	if store.local_storage_status().is_some() {
		format!("WITH INDEX {index}")
	} else {
		String::new()
	}
}

/// Defines a typed public SDK fixture and returns its dictionary identities.
pub fn people(store: &Datastore) -> (PropertyKeyId, PropertyKeyId) {
	let mut schema = store.begin();
	schema
		.define_class(
			ClassSchema::new("person", AtomKind::Node, [
				ClassField::new("name", Type::String),
				ClassField::new("age", Type::Int),
			])
			.unwrap(),
		)
		.unwrap();
	schema.commit().unwrap();
	let dictionary = store.property_dictionary();
	(dictionary.get("name").unwrap(), dictionary.get("age").unwrap())
}

/// Creates an unattached record using resolved SDK property keys.
pub fn create_person(store: &Datastore, id: &str, name: &str, age: i64) {
	let dictionary = store.property_dictionary();
	let mut properties = PropertyMap::new();
	properties.insert(dictionary.get("name").unwrap(), name);
	properties.insert(dictionary.get("age").unwrap(), age);
	let mut write = store.begin();
	write.create_atom(record(id), properties).unwrap();
	write.commit().unwrap();
}

#![cfg(all(windows, feature = "native-async-io"))]
#![allow(
	clippy::unwrap_used,
	reason = "Native I/O contract test fails on the first unexpected error"
)]

use std::path::PathBuf;

use ferros_grynn::{
	datastore::{CheckpointMode, Datastore, IoMode, LocalOptions},
	values::{Object, Uuid},
};

/// Removes the test database once all native handles have been released.
struct Directory(PathBuf);

impl Drop for Directory {
	fn drop(&mut self) {
		drop(std::fs::remove_dir_all(&self.0));
	}
}

#[test]
fn geometry_native_async_index_matches_scan_after_mutation_and_reopen() {
	let directory = Directory(
		std::env::temp_dir()
			.join(format!("fgdb-{:016x}", Uuid::now_v7().as_inner().as_u128() as u64)),
	);
	let mut options = LocalOptions::default();
	options.io_mode = IoMode::NativeAsync;
	options.buffer_pool_pages = 16;
	let store = Datastore::create_local(&directory.0, options).unwrap();
	let setup = store
		.execute(
			"DEFINE CLASS place TYPE NODE SCHEMAFULL; DEFINE FIELD shape ON CLASS place TYPE \
			 geometry;
		 CREATE place:a SET shape = <geometry>{type:'Point', coordinates:[1,1]};
		 CREATE place:b SET shape = <geometry>{type:'Point', coordinates:[8,8]};
		 DEFINE INDEX by_shape ON CLASS place FIELDS shape GEOMETRY SCOPE BASE;",
			&Object::new(),
		)
		.unwrap();
	for result in setup {
		assert!(result.result.is_ok(), "{:?}", result.result);
	}
	let indexed = "SELECT VALUE id FROM place WITH INDEX by_shape WHERE shape INSIDE \
	               <geometry>{type:'Polygon',coordinates:[[[0,0],[5,0],[5,5],[0,5],[0,0]]]} ORDER \
	               BY id;";
	let scan = indexed.replace("WITH INDEX by_shape", "WITH NOINDEX");
	let value = |store: &Datastore, text: &str| {
		store.execute(text, &Object::new()).unwrap().remove(0).result.unwrap().into_parts().0
	};
	assert_eq!(value(&store, indexed), value(&store, &scan));
	let mut old = store.begin();
	let before = value(&store, &scan);
	store
		.execute(
			"UPDATE place:b SET shape = <geometry>{type:'Point', coordinates:[2,2]};",
			&Object::new(),
		)
		.unwrap()
		.remove(0)
		.result
		.unwrap();
	assert_eq!(
		old.execute(indexed, &Object::new()).unwrap().remove(0).result.unwrap().into_parts().0,
		before
	);
	drop(old);
	let expected = value(&store, &scan);
	assert_ne!(expected, before);
	assert_eq!(value(&store, indexed), expected);
	assert!(store.checkpoint(CheckpointMode::Restart).unwrap());
	drop(store);
	let reopened = Datastore::open_local(&directory.0, options).unwrap();
	assert_eq!(value(&reopened, indexed), expected);
}

use std::{
	fs,
	path::{Path, PathBuf},
	sync::atomic::{AtomicU64, Ordering},
};

use super::{IMPORT_FAILURE_POINT, ImportFailurePoint};
use crate::{
	datastore::{ClassSchema, Datastore, ImportOptions, LocalOptions},
	graph::AtomKind,
	values::{PropertyMap, RecordId},
};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
	fn new() -> Self {
		Self(
			std::env::temp_dir()
				.join(format!("fgdb-{:016x}", NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed))),
		)
	}
}

impl Drop for TestDirectory {
	fn drop(&mut self) {
		drop(fs::remove_dir_all(&self.0));
	}
}

#[test]
fn every_prepublication_failure_discards_root_and_unreachable_generation() {
	let directory = TestDirectory::new();
	let store = Datastore::create_local(&directory.0, LocalOptions::default()).unwrap();
	let mut schema = store.begin();
	schema.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	schema.commit().unwrap();
	let generation_count = generation_directories(&directory.0);

	for point in [
		ImportFailurePoint::AfterBuild,
		ImportFailurePoint::AfterFileSync,
		ImportFailurePoint::AfterCatalogStage,
		ImportFailurePoint::BeforeWalCommit,
	] {
		IMPORT_FAILURE_POINT.set(point);
		let result = store.import(ImportOptions::default(), |session| {
			session.apply_item(|transaction| {
				transaction.create_atom("person:alice".parse()?, PropertyMap::new())
			})?;
			Ok(())
		});
		IMPORT_FAILURE_POINT.set(ImportFailurePoint::None);

		assert!(result.is_err());
		assert!(
			store.begin().atom(&"person:alice".parse::<RecordId>().unwrap()).unwrap().is_none()
		);
		assert_eq!(generation_directories(&directory.0), generation_count);
	}
}

fn generation_directories(root: &Path) -> usize {
	let mut count = 0;
	for family in ["atoms", "graphs"] {
		let family = root.join(family);
		let Ok(entries) = fs::read_dir(&family) else {
			continue;
		};
		for entry in entries.flatten() {
			let path = entry.path();
			if !path.is_dir() {
				continue;
			}
			if family.ends_with("atoms") {
				count += 1;
			} else if let Ok(generations) = fs::read_dir(path) {
				count += generations.flatten().filter(|entry| entry.path().is_dir()).count();
			}
		}
	}
	count
}

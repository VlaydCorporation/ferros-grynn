#![allow(
	clippy::expect_used,
	clippy::unwrap_used,
	reason = "integration tests fail immediately when public datastore operations return an error"
)]

#[cfg(test)]
mod common;

#[cfg(test)]
mod tests {
	use ferros_grynn::{
		datastore::Datastore,
		graph::{GraphRef, GraphStorageMode},
	};

	use super::common::{assert_people_graph, commit_people_graph, create_uncommitted_graph};

	#[test]
	fn commit_is_visible_through_the_public_graph_api() {
		let store = Datastore::in_memory();
		let (reference, alice, name) = commit_people_graph(&store);

		assert_eq!(reference.storage_mode(), GraphStorageMode::InMemory);
		assert_people_graph(&store, &reference, &alice, name);
	}

	#[test]
	fn dropped_transaction_discards_its_graph() {
		let store = Datastore::in_memory();
		let reference = create_uncommitted_graph(&store);

		assert!(GraphRef::read(&reference, &store.begin()).is_err());
	}
}

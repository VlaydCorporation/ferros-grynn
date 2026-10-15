#![cfg(test)]
#![allow(
	clippy::expect_used,
	clippy::unwrap_used,
	reason = "integration tests fail immediately when public datastore operations return an error"
)]
/// Assertions shared with the root unit-test harness.
#[path = "common/assertions.rs"]
pub mod test_assertions;

#[cfg(test)]
/// Assertions shared by integration-test targets regression tests.
mod common;

#[cfg(test)]
/// Regression tests for in memory datastore behavior.
mod tests {
	use ferros_grynn::{
		datastore::Datastore,
		graph::{GraphRef, GraphStorageMode},
	};

	use super::common::{
		assert_people_graph,
		commit_people_graph,
		create_uncommitted_graph,
		exercise_normalized_atom_model,
	};

	/// Verifies commit is visible through the public graph api.
	#[test]
	fn commit_is_visible_through_the_public_graph_api() {
		let store = Datastore::in_memory();
		let (reference, alice, name) = commit_people_graph(&store);

		assert_eq!(reference.storage_mode(), GraphStorageMode::InMemory);
		assert_people_graph(&store, &reference, &alice, name);
	}

	/// Verifies dropped transaction discards its graph.
	#[test]
	fn dropped_transaction_discards_its_graph() {
		let store = Datastore::in_memory();
		let reference = create_uncommitted_graph(&store);

		assert!(GraphRef::read(&reference, &store.begin()).is_err());
	}

	/// Verifies normalized atom model preserves shared base and isolates graph
	/// state.
	#[test]
	fn normalized_atom_model_preserves_shared_base_and_isolates_graph_state() {
		let store = Datastore::in_memory();
		exercise_normalized_atom_model(&store);
	}

	/// Verifies geometry index is snapshot exact and enforces type on ram
	/// writes.
	#[test]
	fn geometry_index_is_snapshot_exact_and_enforces_type_on_ram_writes() {
		use ferros_grynn::values::{Geometry, Object, Value};
		let store = Datastore::in_memory();
		let setup = store
			.execute(
				"DEFINE CLASS place TYPE NODE SCHEMAFULL; DEFINE FIELD shape ON CLASS place TYPE \
				 geometry; CREATE place:a SET shape = <geometry>{type: 'Point', coordinates: [1, \
				 1]}; CREATE place:b SET shape = <geometry>{type: 'Point', coordinates: [8, 8]}; \
				 DEFINE INDEX by_shape ON CLASS place FIELDS shape GEOMETRY TYPE Point SCOPE BASE;",
				&Object::new(),
			)
			.unwrap();
		for result in setup {
			assert!(result.result.is_ok(), "{:?}", result.result);
		}
		let parameters =
			Object::from_iter([(
				"area",
				Value::Geometry(
					r#"{"type":"Polygon","coordinates":[[[0,0],[5,0],[5,5],[0,5],[0,0]]]}"#
						.parse::<Geometry>()
						.unwrap(),
				),
			)]);
		let indexed =
			"SELECT VALUE id FROM place WITH INDEX by_shape WHERE shape INSIDE $area ORDER BY id;";
		let scan = "SELECT VALUE id FROM place WITH NOINDEX WHERE shape INSIDE $area ORDER BY id;";
		let value = |text| {
			store.execute(text, &parameters).unwrap().remove(0).result.unwrap().into_parts().0
		};
		let expected = value(scan);
		assert_eq!(value(indexed), expected);
		let mut old = store.begin();
		store
			.execute(
				"UPDATE place:b SET shape = <geometry>{type: 'Point', coordinates: [2, 2]};",
				&Object::new(),
			)
			.unwrap()
			.remove(0)
			.result
			.unwrap();
		assert_eq!(
			old.execute(indexed, &parameters).unwrap().remove(0).result.unwrap().into_parts().0,
			expected
		);
		assert_eq!(value(indexed), value(scan));
		let rejected = store
			.execute(
				"UPDATE place:b SET shape = <geometry>{type: 'LineString', coordinates: [[1, 1], \
				 [2, 2]]};",
				&Object::new(),
			)
			.unwrap();
		assert!(rejected[0].result.is_err());
		assert_eq!(value(indexed), value(scan));
	}
}

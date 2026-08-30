use std::any::TypeId;

use ferros_grynn::{
	datastore::Datastore,
	gql::ParserSettings,
	gql_status::StatusCode,
	graph::{Graph, Mutable},
	values::Value,
};

#[test]
fn public_facades_remain_importable_after_internal_moves() {
	let _store = Datastore::in_memory();
	let _settings = ParserSettings::default();
	let _value = Value::None;
	let _graph_type = TypeId::of::<Graph<Mutable>>();
	assert!("FG-00000".parse::<StatusCode>().is_ok());
}

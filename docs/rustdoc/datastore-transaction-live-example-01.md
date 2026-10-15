```
use ferros_grynn::{
	datastore::{ClassSchema, Datastore},
	graph::AtomKind,
	live::{LiveOptions, LiveQuery, LiveSource, LiveState},
};
# fn example() -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
let store = Datastore::in_memory();
let mut tx = store.begin();
let class = tx.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
let id = tx.register_live(LiveQuery::new(LiveSource::Class(class)), LiveOptions::default())?;
tx.commit()?;
let info = store.begin().live_info(id)?;
assert_eq!(info.state(), LiveState::Active);
# Ok(()) }
# example()?;
# Ok::<(), ferros_grynn::gql_status::FerrosGrynnError>(())
```

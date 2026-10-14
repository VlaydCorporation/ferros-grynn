//! Executes the SDK and GrynnQL examples from the project README.

use ferros_grynn::{datastore::Datastore, gql_status::FerrosGrynnResult, values::Object};

/// Runs the documented examples against an in-memory datastore.
///
/// # Errors
/// Returns a datastore or statement error if either example fails.
#[allow(clippy::print_stdout, reason = "this example displays query results")]
fn main() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();

	for statement in store.execute("RETURN 42;", &Object::new())? {
		println!("{}", statement.result?.value());
	}

	for statement in store.execute(include_str!("quick_look.gql"), &Object::new())? {
		println!("{}", statement.result?.value());
	}

	Ok(())
}
